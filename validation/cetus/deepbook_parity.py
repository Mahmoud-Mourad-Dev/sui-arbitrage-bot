"""Phase 3 parity for DeepBook: local orderbook depth-walk vs the authoritative
on-chain quoter (pool::get_quote_quantity_out). Read-only."""

import statistics

import deepbook_rpc as db

SIZES_BASE = {  # native base amounts spanning small -> book-crossing
    "SUI_USDC": [1_000_000_000, 10_000_000_000, 100_000_000_000, 1_000_000_000_000, 5_000_000_000_000],
    "DEEP_SUI": [10_000_000, 100_000_000, 1_000_000_000, 10_000_000_000, 100_000_000_000],
    "DEEP_USDC": [10_000_000, 100_000_000, 1_000_000_000, 10_000_000_000, 100_000_000_000],
}


def main():
    abs_rel = []
    covered_n = total_n = 0
    for name, p in db.POOLS.items():
        isv = db.get_isv(p["id"])
        level2 = db.level2_from_mid(p["id"], isv, p["base"], p["quote"], ticks=100)
        print(f"\n{name}: bid levels={len(level2[0])} ask levels={len(level2[2])}")
        for sz in SIZES_BASE.get(name, []):
            total_n += 1
            auth = db.quote_base_to_quote(p["id"], isv, p["base"], p["quote"], sz)["quote_out"]
            dw, covered = db.depth_walk_base_to_quote(level2, sz)
            if not covered:
                print(f"  size {sz}: book not fully covered by fetched ticks (skip)")
                continue
            covered_n += 1
            rel = abs(dw - auth) / auth if auth else 0.0
            abs_rel.append(rel)
            print(f"  size {sz:>16}: auth={auth:>14} depth_walk={dw:>14} rel_err={rel:.3e}")
    print("\n=== DeepBook parity (depth-walk vs authoritative) ===")
    print(f"coverage: {covered_n}/{total_n} sizes within fetched book")
    if abs_rel:
        s = sorted(abs_rel)
        print(f"rel_err: mean={statistics.mean(s):.3e} median={statistics.median(s):.3e} "
              f"p95={s[min(len(s)-1,int(len(s)*0.95))]:.3e} max={max(s):.3e}")
        ok = max(s) < 1e-3
        print("PARITY:", "PASS (depth-walk matches authoritative)" if ok else "review")


if __name__ == "__main__":
    main()
