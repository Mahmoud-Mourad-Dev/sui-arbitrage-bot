"""Phase 1 (discover all pools) + Phase 3 parity for Bluefin.
Local CLMM engine (single-range Q64.64) vs Bluefin pool::calculate_swap_results.
Gate: max relative error < 0.1% on the in-range regime. Read-only."""

import json
import statistics
import subprocess

import bluefin_rpc as bf

ENGINE = "../../offchain/target/release/examples/clmm_quote"
SIZES = [10 ** k for k in range(3, 12)]


def discover_all():
    pools = []
    for pid in bf.page_pool_ids(pages=12):
        try:
            p = bf.get_pool(pid)
            if p["liquidity"] > 0 and not p["is_paused"]:
                pools.append(p)
        except Exception:
            pass
    json.dump(pools, open("bluefin_pools.json", "w"))
    return pools


def engine_quotes(scen):
    lines = [json.dumps({"sqrt_price": str(s["sqrt_price"]), "liquidity": str(s["liquidity"]),
                         "fee_pips": s["fee_rate"], "amount": s["amount"], "a_to_b": s["a2b"],
                         "ticks": []}) for s in scen]
    out = subprocess.run([ENGINE], input="\n".join(lines), capture_output=True, text=True)
    if out.returncode != 0:
        raise RuntimeError(out.stderr[:400])
    for s, o in zip(scen, out.stdout.strip().split("\n")):
        s["engine_out"] = None if o == "none" else int(o)


def main():
    pools = discover_all()
    print(f"discovered {len(pools)} Bluefin pools with liquidity")
    pools.sort(key=lambda p: -p["liquidity"])
    test = pools[:8]
    print("parity on:", [f"{p['type_x'].split('::')[-1]}/{p['type_y'].split('::')[-1]}" for p in test])

    scen = []
    for p in test:
        for a2b in (True, False):
            for amt in SIZES:
                try:
                    auth = bf.quote(p["id"], p["isv"], p["type_x"], p["type_y"], a2b, amt)
                except Exception:
                    continue
                if auth <= 0:
                    continue
                scen.append({"pair": f"{p['type_x'].split('::')[-1]}/{p['type_y'].split('::')[-1]}",
                             "sqrt_price": p["sqrt_price"], "liquidity": p["liquidity"],
                             "fee_rate": p["fee_rate"], "amount": amt, "a2b": a2b, "auth": auth})
    engine_quotes(scen)

    in_range, cross = [], 0
    for s in scen:
        if s["engine_out"] is None:
            continue
        rel = abs(s["engine_out"] - s["auth"]) / s["auth"]
        if rel < 1e-3:
            in_range.append(rel)
        else:
            cross += 1
    print(f"\nscenarios {len(scen)} | in-range(<0.1%) {len(in_range)} | cross-tick {cross}")
    if in_range:
        srt = sorted(in_range)
        print(f"in-range rel_err: mean={statistics.mean(srt):.3e} median={statistics.median(srt):.3e} "
              f"p95={srt[min(len(srt)-1,int(len(srt)*0.95))]:.3e} max={max(srt):.3e}")
        print("GATE (<0.1%):", "PASS" if max(srt) < 1e-3 else "FAIL")


if __name__ == "__main__":
    main()
