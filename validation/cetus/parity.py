"""Differential parity test: Rust CLMM engine vs Cetus on-chain quoter.

Per pool, ONE dev-inspect returns (current_sqrt_price, liquidity, fee_rate) plus a
calculate_swap_result for every (size, direction) — all from a single atomic state
snapshot. The same snapshot is run through the Rust engine (examples/clmm_quote)
and outputs are compared. Tick arrays are fetched separately (swaps never change
tick liquidity_net, so they stay valid across the snapshot). NEVER submits a tx.
"""

import json
import statistics
import subprocess
import sys

import cetus_rpc as c

ENGINE = "../../offchain/target/release/examples/clmm_quote"
SIZES_USD = [1, 2, 3, 5, 10, 20, 30, 50, 100, 200, 300, 500,
             1000, 2000, 3000, 5000, 10000, 20000, 30000, 50000]
MAX_TICKS_DIR = 400

cfg = json.load(open("pools_config.json"))
SUI_USD = cfg["sui_usd"]
STABLES = set(cfg["stables"])


def usd_sides(p):
    a, b = p["sym_a"].lower(), p["sym_b"].lower()
    s = p["sqrt_price"] / 2 ** 64
    price_b_per_a = s * s * 10 ** (p["dec_a"] - p["dec_b"])
    usd_a = SUI_USD if a == "sui" else (1.0 if a in STABLES else None)
    usd_b = SUI_USD if b == "sui" else (1.0 if b in STABLES else None)
    if usd_b is not None and usd_a is None and price_b_per_a > 0:
        usd_a = usd_b * price_b_per_a
    if usd_a is not None and usd_b is None and price_b_per_a > 0:
        usd_b = usd_a / price_b_per_a
    return usd_a, usd_b


def native_amount(usd_target, usd_whole, dec):
    if not usd_whole or usd_whole <= 0:
        return 0
    return int(usd_target / usd_whole * 10 ** dec)


def ticks_for_dir(ticks, sqrt_price, a2b):
    if a2b:
        t = sorted([x for x in ticks if x["sqrt_price"] < sqrt_price], key=lambda x: -x["sqrt_price"])
    else:
        t = sorted([x for x in ticks if x["sqrt_price"] > sqrt_price], key=lambda x: x["sqrt_price"])
    return t[:MAX_TICKS_DIR]


def collect(max_pools):
    samples = []
    for i, p in enumerate(cfg["pools"][:max_pools]):
        usd_a, usd_b = usd_sides(p)
        if usd_a is None or usd_b is None:
            print(f"  [{i}] skip {p['sym_a']}/{p['sym_b']} (no USD anchor)")
            continue

        scen = []  # (a2b, amount, size_usd)
        for usd in SIZES_USD:
            for a2b in (True, False):
                dec = p["dec_a"] if a2b else p["dec_b"]
                uw = usd_a if a2b else usd_b
                amt = native_amount(usd, uw, dec)
                if amt > 0:
                    scen.append((a2b, amt, usd))
        if not scen:
            continue

        try:
            ticks = c.fetch_all_ticks(p["id"], p["init_shared_version"], p["type_a"], p["type_b"])
            snap, quotes = c.snapshot_and_quotes(
                p["id"], p["init_shared_version"], p["type_a"], p["type_b"],
                [(a, amt) for (a, amt, _u) in scen],
            )
        except Exception as e:
            print(f"  [{i}] skip {p['sym_a']}/{p['sym_b']}@{p['fee_rate']}: {e}")
            continue

        cross = 0
        for (a2b, amt, usd), q in zip(scen, quotes):
            dirt = ticks_for_dir(ticks, snap["sqrt_price"], a2b)
            if q["n_steps"] > 1:
                cross += 1
            samples.append({
                "pool": p["id"], "pair": f"{p['sym_a']}/{p['sym_b']}", "fee": snap["fee_rate"],
                "spacing": p["tick_spacing"], "size_usd": usd, "a2b": a2b, "amount": amt,
                "cetus_out": q["amount_out"], "n_steps": q["n_steps"], "is_exceed": q["is_exceed"],
                "sqrt_price": snap["sqrt_price"], "liquidity": snap["liquidity"],
                "ticks": [[str(t["sqrt_price"]), str(t["liquidity_net"])] for t in dirt],
            })
        print(f"  [{i}] {p['sym_a']}/{p['sym_b']}@{p['fee_rate']}: {len(scen)} samples ({cross} cross-tick), "
              f"{len(ticks)} ticks")
    return samples


def run_engine(samples):
    if not samples:
        return
    lines = [json.dumps({
        "sqrt_price": str(s["sqrt_price"]), "liquidity": str(s["liquidity"]),
        "fee_pips": s["fee"], "amount": s["amount"], "a_to_b": s["a2b"], "ticks": s["ticks"],
    }) for s in samples]
    proc = subprocess.run([ENGINE], input="\n".join(lines), capture_output=True, text=True)
    if proc.returncode != 0:
        raise RuntimeError(f"engine failed: {proc.stderr[:500]}")
    outs = proc.stdout.strip().split("\n")
    assert len(outs) == len(samples), f"{len(outs)} outs vs {len(samples)}"
    for s, o in zip(samples, outs):
        s["engine_out"] = None if o == "none" else int(o)


def stat_block(name, vals):
    vals = sorted(vals)
    n = len(vals)
    p95 = vals[min(n - 1, int(n * 0.95))]
    print(f"  {name}: n={n} mean={statistics.mean(vals):.6g} median={statistics.median(vals):.6g} "
          f"p95={p95:.6g} max={max(vals):.6g}")


def main():
    max_pools = int(sys.argv[1]) if len(sys.argv) > 1 else len(cfg["pools"])
    print(f"collecting from up to {max_pools} pools...")
    samples = collect(max_pools)
    print(f"\ntotal samples: {len(samples)}")
    run_engine(samples)

    abs_err, rel_err, fails = [], [], []
    excl_exceed = n_cross = 0
    for s in samples:
        if s["is_exceed"]:
            excl_exceed += 1
            continue
        if s["engine_out"] is None:
            fails.append(s)
            continue
        if s["n_steps"] > 1:
            n_cross += 1
        a = abs(s["engine_out"] - s["cetus_out"])
        r = a / s["cetus_out"] if s["cetus_out"] > 0 else (0.0 if a == 0 else 1.0)
        s["abs_err"], s["rel_err"] = a, r
        abs_err.append(a)
        rel_err.append(r)
        if not (a <= 1 or r < 1e-6):
            fails.append(s)

    json.dump(samples, open("parity_results.json", "w"))
    graded = len(abs_err)
    print(f"\ngraded (excl is_exceed): {graded} | cross-tick among graded: {n_cross} | is_exceed excluded: {excl_exceed}")
    if graded:
        stat_block("abs_error (token units)", abs_err)
        stat_block("rel_error", rel_err)
    print(f"\nFAILURES (not <=1 unit and not rel<1e-6): {len(fails)}")
    for s in fails[:25]:
        print("  FAIL", s["pair"], "fee", s["fee"], "a2b", s["a2b"], "usd", s["size_usd"],
              "amt", s["amount"], "engine", s.get("engine_out"), "cetus", s["cetus_out"],
              "steps", s["n_steps"], "abs", s.get("abs_err"), "rel", s.get("rel_err"))
    print("\nACCEPTANCE:", "PASS" if (not fails and graded > 0) else "FAIL")


if __name__ == "__main__":
    main()
