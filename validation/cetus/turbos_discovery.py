"""Phase 3 — discover all active Turbos pools (read-only). Outputs pool id, pair,
fee tier, liquidity, sqrt price, tick, tick spacing + coverage stats."""

import json

import cetus_rpc as c
import turbos_rpc as t


def discover():
    ids = t.page_pool_ids(pages=12)
    pools = []
    for i in range(0, len(ids), 50):
        metas = c.rpc("sui_multiGetObjects", [ids[i:i + 50], {"showType": True, "showContent": True, "showOwner": True}])
        for o in metas:
            try:
                d = o["data"]
                p = t.get_pool(d["objectId"])
                p["sym_a"] = p["type_a"].split("::")[-1]
                p["sym_b"] = p["type_b"].split("::")[-1]
                p["fee_tier"] = p["fee_type"].split("::")[-1]
                pools.append(p)
            except Exception:
                pass
    json.dump(pools, open("turbos_pools.json", "w"))
    return pools


def main():
    pools = discover()
    active = [p for p in pools if p["liquidity"] > 0 and p["unlocked"]]
    print(f"discovered {len(pools)} pools | active w/ liquidity: {len(active)}")
    from collections import Counter
    print("fee tiers:", dict(Counter(p["fee_tier"] for p in active)))
    print("tick spacings:", dict(Counter(p["tick_spacing"] for p in active)))
    liqs = sorted((p["liquidity"] for p in active), reverse=True)
    print(f"liquidity: max={liqs[0]:.3e} median={liqs[len(liqs)//2]:.3e} min={liqs[-1]:.3e}")
    print(f"\n{'pool':<14}{'pair':<18}{'fee':<12}{'spacing':>8}{'liquidity':>16}{'tick':>9}")
    for p in sorted(active, key=lambda x: -x["liquidity"]):
        print(f"{p['id'][:12]}  {p['sym_a']+'/'+p['sym_b']:<18}{p['fee_tier']:<12}{p['tick_spacing']:>8}"
              f"{p['liquidity']:>16}{str(p['tick']):>9}")


if __name__ == "__main__":
    main()
