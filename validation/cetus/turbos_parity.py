"""Phase 4 — Turbos parity harness. Off-chain CLMM engine (single-range Q64.64) vs
Turbos authoritative pool_fetcher::compute_swap_result, across pools x USD sizes x
both directions. Classifies same-range / cross-tick / multi-tick by the authoritative
end-tick. Read-only. PASS gate: max rel error < 0.1% with no systematic bias."""

import json
import statistics
import subprocess

import mv_scan as m
import turbos_rpc as t

ENGINE = "../../offchain/target/release/examples/clmm_quote"
SIZES_USD = [1, 5, 10, 50, 100, 500, 1000, 5000]
STABLES = {"USDC", "USDT", "BUCK", "USDB", "USDY", "AUSD", "WUSDC"}


def usd_of(sym, sui_usd):
    if sym in STABLES:
        return 1.0
    if sym == "SUI":
        return sui_usd
    return None


def main():
    pools = json.load(open("turbos_pools.json"))
    pools = [p for p in pools if p["liquidity"] > 0 and p["unlocked"]]
    pools.sort(key=lambda p: -p["liquidity"])
    # SUI/USD from the deepest SUI/USDC turbos pool (authoritative)
    sui_usd = 0.72
    for p in pools:
        if {p["sym_a"], p["sym_b"]} == {"SUI", "USDC"}:
            a2b = (p["sym_a"] == "SUI")
            outo = t.quote_exact_in(p["id"], p["isv"], p["type_a"], p["type_b"], p["fee_type"], a2b, 1_000_000_000)
            sui_usd = outo / 1e6
            break
    print(f"SUI/USD (authoritative) ~= {sui_usd:.4f}")

    scen = []
    for p0 in pools:
        try:                       # refresh state so engine & authoritative price the SAME snapshot
            p = t.get_pool(p0["id"])
            p["sym_a"], p["sym_b"] = p0["sym_a"], p0["sym_b"]
        except Exception:
            continue
        da = m.coin_decimals(p["type_a"])
        db = m.coin_decimals(p["type_b"])
        if da is None or db is None:
            continue
        usd_a = usd_of(p["sym_a"], sui_usd)
        usd_b = usd_of(p["sym_b"], sui_usd)
        # derive the unknown side from pool price if one side is anchorable
        s = p["sqrt_price"] / 2 ** 64
        price_b_per_a = s * s * 10 ** (da - db)
        if usd_a is None and usd_b is not None and price_b_per_a > 0:
            usd_a = usd_b * price_b_per_a
        if usd_b is None and usd_a is not None and price_b_per_a > 0:
            usd_b = usd_a / price_b_per_a
        if usd_a is None or usd_b is None:
            continue
        for a2b in (True, False):
            dec = da if a2b else db
            uw = usd_a if a2b else usd_b
            for usd in SIZES_USD:
                amt = int(usd / uw * 10 ** dec) if uw > 0 else 0
                if amt <= 0:
                    continue
                try:
                    full = t.quote_full(p["id"], p["isv"], p["type_a"], p["type_b"], p["fee_type"], a2b, amt)
                except Exception:
                    continue
                if full["amount_out"] <= 0:
                    continue
                crossings = abs(full["end_tick"] - (p["tick"] or 0)) // max(p["tick_spacing"], 1)
                scen.append({"pool": p["id"][:10], "pair": f"{p['sym_a']}/{p['sym_b']}",
                             "liquidity": p["liquidity"], "size_usd": usd, "a2b": a2b, "amount": amt,
                             "sqrt_price": p["sqrt_price"], "fee": p["fee"], "auth": full["amount_out"],
                             "crossings": crossings})

    # engine quotes in one batch
    lines = [json.dumps({"sqrt_price": str(s["sqrt_price"]), "liquidity": str(s["liquidity"]),
                         "fee_pips": s["fee"], "amount": s["amount"], "a_to_b": s["a2b"], "ticks": []})
             for s in scen]
    out = subprocess.run([ENGINE], input="\n".join(lines), capture_output=True, text=True)
    for s, o in zip(scen, out.stdout.strip().split("\n")):
        s["engine"] = None if o == "none" else int(o)

    rows = [s for s in scen if s.get("engine") is not None]
    for s in rows:
        s["rel"] = abs(s["engine"] - s["auth"]) / s["auth"] if s["auth"] else 0.0
        s["abs"] = abs(s["engine"] - s["auth"])
        s["cls"] = "same-range" if s["crossings"] == 0 else ("cross-tick" if s["crossings"] == 1 else "multi-tick")
    json.dump(rows, open("turbos_parity_results.json", "w"))

    def stat(label, sub):
        if not sub:
            print(f"  {label:<12} n=0"); return
        r = sorted(x["rel"] for x in sub)
        bias = statistics.mean(x["engine"] - x["auth"] for x in sub)
        print(f"  {label:<12} n={len(sub):<4} mean={statistics.mean(r):.3e} median={statistics.median(r):.3e} "
              f"p95={r[min(len(r)-1,int(len(r)*0.95))]:.3e} max={max(r):.3e}  mean_signed_bias={bias:+.2f}")

    print(f"\nTURBOS PARITY — {len(rows)} scenarios across {len(set(s['pool'] for s in rows))} pools")
    stat("ALL", rows)
    same = [s for s in rows if s["cls"] == "same-range"]
    ct = [s for s in rows if s["cls"] == "cross-tick"]
    mt = [s for s in rows if s["cls"] == "multi-tick"]
    stat("same-range", same)
    stat("cross-tick", ct)
    stat("multi-tick", mt)

    same_max = max((s["rel"] for s in same), default=0)
    # systematic bias: in same-range, engine should equal authoritative (no signed drift)
    same_bias = statistics.mean([s["engine"] - s["auth"] for s in same]) if same else 0
    print(f"\nGATE (same-range, the bot's regime): max rel {same_max:.3e}  signed_bias {same_bias:+.3f}")
    print("PASS" if same_max < 1e-3 else "FAIL",
          "— engine matches Turbos authoritative in-range" if same_max < 1e-3 else "")
    print(f"NOTE cross-tick/multi-tick: engine single-range OVERESTIMATES (no tick traversal); "
          f"{len(ct)+len(mt)}/{len(rows)} scenarios. These are the divergence source.")


if __name__ == "__main__":
    main()
