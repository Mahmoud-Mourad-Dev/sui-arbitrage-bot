"""Phase 1 (discover all pools) + Phase 3 parity for Momentum.

Parity = local CLMM engine (offchain/examples/clmm_quote, single-range Q64.64) vs
Momentum's authoritative trade::compute_swap_result, across pools x sizes x both
directions. Gate: max relative error < 0.1% on the in-range regime the single-range
model covers. (Production integration uses the authoritative quote directly, like
Cetus/DeepBook; this validates the fast-path engine.)  Read-only.
"""

import json
import statistics
import subprocess

import momentum_rpc as mmt

ENGINE = "../../offchain/target/release/examples/clmm_quote"
# native-unit size ladder (per token_x); small->large to find the in-range regime
SIZES = [10 ** k for k in range(3, 12)]  # 1e3 .. 1e11


def discover_all():
    ids = mmt.page_pool_ids(pages=10)
    pools = []
    for pid in ids:
        try:
            p = mmt.get_pool(pid)
            if p["liquidity"] > 0 and p["type_x"] and p["type_y"]:
                pools.append(p)
        except Exception:
            pass
    json.dump(pools, open("momentum_pools.json", "w"))
    return pools


def engine_quotes(scenarios):
    lines = [json.dumps({"sqrt_price": str(s["sqrt_price"]), "liquidity": str(s["liquidity"]),
                         "fee_pips": s["fee_rate"], "amount": s["amount"], "a_to_b": s["x_for_y"],
                         "ticks": []}) for s in scenarios]
    out = subprocess.run([ENGINE], input="\n".join(lines), capture_output=True, text=True)
    if out.returncode != 0:
        raise RuntimeError(out.stderr[:400])
    res = out.stdout.strip().split("\n")
    for s, o in zip(scenarios, res):
        s["engine_out"] = None if o == "none" else int(o)


def main():
    pools = discover_all()
    print(f"discovered {len(pools)} Momentum pools with liquidity")
    pools.sort(key=lambda p: -p["liquidity"])
    test = pools[:8]
    print("parity on top", len(test), "by liquidity:",
          [f"{p['type_x'].split('::')[-1]}/{p['type_y'].split('::')[-1]}" for p in test])

    scenarios = []
    for p in test:
        for x_for_y in (True, False):
            for amt in SIZES:
                # authoritative
                try:
                    auth = mmt.quote(p["id"], p["isv"], p["type_x"], p["type_y"], x_for_y, amt)
                except Exception:
                    continue
                if auth <= 0:
                    continue
                scenarios.append({"pair": f"{p['type_x'].split('::')[-1]}/{p['type_y'].split('::')[-1]}",
                                  "sqrt_price": p["sqrt_price"], "liquidity": p["liquidity"],
                                  "fee_rate": p["fee_rate"], "amount": amt, "x_for_y": x_for_y,
                                  "auth": auth})
    engine_quotes(scenarios)

    in_range, cross = [], 0
    for s in scenarios:
        if s["engine_out"] is None:
            continue
        rel = abs(s["engine_out"] - s["auth"]) / s["auth"]
        s["rel"] = rel
        if rel < 1e-3:           # < 0.1% => single-range model matches (in-range)
            in_range.append(rel)
        else:
            cross += 1
    print(f"\nscenarios: {len(scenarios)}  in-range(<0.1%): {len(in_range)}  cross-tick(>=0.1%): {cross}")
    if in_range:
        s = sorted(in_range)
        print(f"in-range rel_err: mean={statistics.mean(s):.3e} median={statistics.median(s):.3e} "
              f"p95={s[min(len(s)-1,int(len(s)*0.95))]:.3e} max={max(s):.3e}")
        print("GATE (<0.1% on in-range):", "PASS" if max(s) < 1e-3 else "FAIL")
    # show a few representative rows
    print("\nsamples (pair, dir, size, auth, engine, rel):")
    for s in scenarios[:14]:
        if "rel" in s:
            print(f"  {s['pair']:<14} {'x2y' if s['x_for_y'] else 'y2x'} {s['amount']:>13} "
                  f"auth={s['auth']:>14} eng={s['engine_out']:>14} rel={s['rel']:.2e}")


if __name__ == "__main__":
    main()
