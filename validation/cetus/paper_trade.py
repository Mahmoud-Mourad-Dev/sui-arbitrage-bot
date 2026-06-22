"""24h-capable PAPER-TRADING framework for Sui DEX arbitrage. READ-ONLY.

Each round:
  1. scan Cetus + Turbos + Kriya (reuses mv_scan detection),
  2. detect candidate cycles (model "expected" profit, optimal size),
  3. DRY-RUN VALIDATE each unique candidate against authoritative on-chain quotes:
       - Cetus legs: pool::calculate_swap_result via dev-inspect (tick-aware),
       - Kriya legs: exact constant-product (Kriya's own invariant),
       - Turbos legs: engine single-range estimate (flagged: not independently
         dry-run-validated -> sample marked authoritative=false),
  4. record expected vs simulated (dry-run) profit + would_execute decision,
  5. checkpoint to paper_trades.jsonl.

NEVER builds or submits a transaction. dev-inspect only simulates.

Run:   python3 paper_trade.py run <minutes>
Report: python3 paper_trade.py report
"""

import json
import os
import sys
import time

import cetus_rpc as c
import mv_scan as m


MIN_POOL_USD = 5000  # ignore pools with < ~$5k on the hub side (thin/artifact prices)


def hub_usd_value(p, sui_usd):
    """USD value of the pool's hub-token (SUI/stable) side reserves, or None."""
    rx, ry = m.virtual_reserves(p)
    vb = m.usd_of(p["sym_b"], sui_usd)
    if vb is not None:
        return ry / 10 ** p["dec_b"] * vb
    va = m.usd_of(p["sym_a"], sui_usd)
    if va is not None:
        return rx / 10 ** p["dec_a"] * va
    return None


def filter_depth(pools, sui_usd, min_usd=MIN_POOL_USD):
    out = []
    for p in pools:
        v = hub_usd_value(p, sui_usd)
        if v is None or v >= min_usd:  # keep deep hub pools; drop thin ones
            out.append(p)
    return out


def canonical(cyc):
    """Rotation-invariant key for a directed cycle (dedupe rotations)."""
    seq = [(e["pool"], e["a2b"]) for e in cyc]
    rots = [tuple(seq[i:] + seq[:i]) for i in range(len(seq))]
    return min(rots)


def dryrun_cycle(cyc, amount_native):
    """Chain authoritative per-hop quotes. Returns (final_native, authoritative)."""
    x = float(amount_native)
    authoritative = True
    for e in cyc:
        if x <= 0:
            return 0.0, authoritative
        if e["venue"] == "cetus" and e.get("isv"):
            q = c.cetus_quote(e["pool"], e["isv"], e["type_a"], e["type_b"], e["a2b"], int(x))
            x = float(q["amount_out"])
        elif e["venue"] == "kriya":
            x = m.cp_out(x, e["rin"], e["rout"], e["fee"])  # exact CP == Kriya invariant
        else:  # turbos (or cetus missing isv): engine single-range estimate
            x = m.cp_out(x, e["rin"], e["rout"], e["fee"])
            authoritative = False
    return x, authoritative


def run(minutes):
    if not os.path.exists("mv_pools.json"):
        m.discover()
    base_pools = json.load(open("mv_pools.json"))
    id_venue = {p["id"]: p["venue"] for p in base_pools}
    deadline = time.time() + minutes * 60
    logf = open("paper_trades.jsonl", "a")
    rnd = 0
    while time.time() < deadline:
        rnd += 1
        t0 = time.time()
        try:
            pools = m.refresh(id_venue)
        except Exception as e:
            print("refresh error", e); time.sleep(10); continue
        sui_usd = m.sui_usd_from(pools)
        if not sui_usd:
            time.sleep(10); continue
        pools = filter_depth(pools, sui_usd)
        edges = m.build_edges(pools)
        adj = m.adjacency(edges)
        bases = {}
        for p in pools:
            for sym, ct, dec in ((p["sym_a"], p["type_a"], p["dec_a"]), (p["sym_b"], p["type_b"], p["dec_b"])):
                if sym in m.HUB_SYMS and ct not in bases:
                    bases[ct] = (sym, dec)

        seen = set()
        detected = validated = execable = 0
        for base_ct, (bsym, bdec) in bases.items():
            ub = m.usd_of(bsym, sui_usd)
            if not ub:
                continue
            for cyc in m.find_cycles(adj, base_ct, 4):
                if m.zero_slip_edge(cyc) <= 0:
                    continue
                key = canonical(cyc)
                if key in seen:
                    continue
                seen.add(key)
                detected += 1
                hops = len(cyc)
                gas_usd = (m.GAS_SUI_BASE + m.GAS_SUI_PER_HOP * hops) * sui_usd
                exp = m.best_net(cyc, bdec, ub, gas_usd)  # model expectation + optimal size
                amt0 = exp["input_usd"] / ub * 10 ** bdec
                try:
                    final, auth = dryrun_cycle(cyc, amt0)
                except Exception as e:
                    print("dryrun error", e); continue
                validated += 1
                sim_gross = (final - amt0) / 10 ** bdec * ub
                sim_net = sim_gross - gas_usd
                if sim_net > 0:
                    execable += 1
                logf.write(json.dumps({
                    "ts": time.time(), "round": rnd, "base": bsym, "hops": hops,
                    "edge_bps": round(m.zero_slip_edge(cyc) * 10000, 3),
                    "path": [e["sym_from"] for e in cyc] + [bsym],
                    "venues": [e["venue"] for e in cyc],
                    "size_usd": exp["input_usd"],
                    "expected_gross_usd": round(exp["gross_usd"], 6),
                    "expected_net_usd": round(exp["net_usd"], 6),
                    "simulated_gross_usd": round(sim_gross, 6),
                    "simulated_net_usd": round(sim_net, 6),
                    "error_usd": round(exp["net_usd"] - sim_net, 6),
                    "authoritative": auth, "would_execute": sim_net > 0,
                }) + "\n")
        logf.flush()
        print(f"round {rnd} +{int(time.time()-t0)}s SUI=${sui_usd:.4f} detected={detected} "
              f"dryrun_validated={validated} would_execute={execable}")
        time.sleep(15)
    logf.close()
    print("paper-trading window complete, rounds:", rnd)


def report():
    import statistics
    rows = [json.loads(l) for l in open("paper_trades.jsonl")]
    if not rows:
        print("no records"); return
    ts = [r["ts"] for r in rows]
    span_h = (max(ts) - min(ts)) / 3600 or 1e-9
    auth = [r for r in rows if r["authoritative"]]
    exe = [r for r in rows if r["would_execute"]]
    print(f"records: {len(rows)} | window: {span_h:.3f} h | authoritative dry-runs: {len(auth)}")
    print(f"detections/hour: {len(rows)/span_h:.1f} | would-execute (sim_net>0): {len(exe)} "
          f"({len(exe)/span_h:.2f}/h)")

    # model fidelity: |expected_net - simulated_net| on authoritative dry-runs
    if auth:
        err = sorted(abs(r["error_usd"]) for r in auth)
        rel = []
        for r in auth:
            denom = max(abs(r["simulated_net_usd"]), 1e-9)
            rel.append(abs(r["error_usd"]) / denom)
        print(f"model vs dry-run |error| ($): mean={statistics.mean(err):.6f} median={statistics.median(err):.6f} "
              f"p95={err[min(len(err)-1,int(len(err)*0.95))]:.6f} max={max(err):.6f}")
        # false positives: model says profit, dry-run says loss
        fp = sum(1 for r in auth if r["expected_net_usd"] > 0 >= r["simulated_net_usd"])
        fn = sum(1 for r in auth if r["expected_net_usd"] <= 0 < r["simulated_net_usd"])
        print(f"model false-positives caught by dry-run: {fp} | false-negatives: {fn}")

    if exe:
        nets = sorted(r["simulated_net_usd"] for r in exe)
        print("PROFITABLE (dry-run net>0):")
        print(f"  net $: mean={statistics.mean(nets):.4f} median={statistics.median(nets):.4f} "
              f"p95={nets[min(len(nets)-1,int(len(nets)*0.95))]:.4f} max={max(nets):.4f} min={min(nets):.4f}")
        total = sum(nets)
        print(f"  paper P&L over window: ${total:.4f} -> ${total/span_h*24:.2f}/day -> ${total/span_h*24*7:.2f}/week")
        toks = {}
        for r in exe:
            for s in r["path"]:
                toks[s] = toks.get(s, 0) + 1
        print("  tokens involved:", dict(sorted(toks.items(), key=lambda kv: -kv[1])))
    else:
        print("PROFITABLE opportunities after gas: 0 (no positive-net dry-runs in window)")


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "run"
    if cmd == "run":
        run(float(sys.argv[2]) if len(sys.argv) > 2 else 60)
    elif cmd == "report":
        report()
