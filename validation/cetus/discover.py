"""Discover live Cetus pools from recent pool events; save a pool config with
state + coin decimals + a SUI/USD anchor (from a USDC/SUI pool) for sizing."""

import json

import cetus_rpc as c

USD_STABLES = {"usdc", "usdt", "buck", "usdy", "ausd", "fdusd"}


def page_pools(pages=6, per=50):
    seen = []
    cursor = None
    for _ in range(pages):
        params = [{"MoveEventModule": {"package": c.CLMM_PKG, "module": "pool"}}, cursor, per, True]
        res = c.rpc("suix_queryEvents", params)
        for e in res.get("data", []):
            pid = e.get("parsedJson", {}).get("pool")
            if pid and pid not in seen:
                seen.append(pid)
        cursor = res.get("nextCursor")
        if not res.get("hasNextPage"):
            break
    return seen


def coin_decimals(coin_type):
    try:
        md = c.rpc("suix_getCoinMetadata", [coin_type])
        return int(md["decimals"]) if md else None
    except Exception:
        return None


def main():
    ids = page_pools()
    print("discovered pool ids:", len(ids))
    metas = c.rpc("sui_multiGetObjects", [ids, {"showType": True, "showContent": True, "showOwner": True}])
    pools = []
    dec_cache = {}
    for o in metas:
        d = o.get("data")
        if not d:
            continue
        try:
            t = d["type"]
            inside = t.split("Pool<", 1)[1].rsplit(">", 1)[0]
            depth = 0
            parts = []
            cur = ""
            for ch in inside:
                if ch == "<":
                    depth += 1
                if ch == ">":
                    depth -= 1
                if ch == "," and depth == 0:
                    parts.append(cur.strip())
                    cur = ""
                else:
                    cur += ch
            parts.append(cur.strip())
            f = d["content"]["fields"]
            liq = int(f["liquidity"])
            if liq == 0:
                continue
            ta, tb = parts[0], parts[1]
            for tk in (ta, tb):
                if tk not in dec_cache:
                    dec_cache[tk] = coin_decimals(tk)
            pools.append({
                "id": d["objectId"],
                "type_a": ta,
                "type_b": tb,
                "sym_a": ta.split("::")[-1],
                "sym_b": tb.split("::")[-1],
                "fee_rate": int(f["fee_rate"]),
                "tick_spacing": int(f["tick_spacing"]),
                "liquidity": liq,
                "sqrt_price": int(f["current_sqrt_price"]),
                "init_shared_version": int(d["owner"]["Shared"]["initial_shared_version"]),
                "dec_a": dec_cache[ta],
                "dec_b": dec_cache[tb],
            })
        except Exception as ex:
            print("skip", d.get("objectId"), ex)

    # SUI/USD from a USDC/SUI pool: price_whole(SUI per USDC) = sqrt^2 * 10^(dec_usdc-dec_sui)
    sui_usd = None
    for p in pools:
        a, b = p["sym_a"].lower(), p["sym_b"].lower()
        if {a, b} & USD_STABLES and "sui" in (a, b):
            s = p["sqrt_price"] / (2 ** 64)
            price_raw = s * s  # native_b per native_a
            if a in USD_STABLES and b == "sui":
                # whole SUI per whole USDC
                sui_per_usdc = price_raw * 10 ** (p["dec_a"] - p["dec_b"])
                sui_usd = 1.0 / sui_per_usdc
            elif b in USD_STABLES and a == "sui":
                usdc_per_sui = price_raw * 10 ** (p["dec_a"] - p["dec_b"])
                sui_usd = usdc_per_sui
            if sui_usd:
                break

    cfg = {"sui_usd": sui_usd, "stables": sorted(USD_STABLES), "pools": pools}
    json.dump(cfg, open("pools_config.json", "w"), indent=1)
    print(f"saved {len(pools)} pools; SUI/USD ~= {sui_usd}")
    from collections import Counter
    pairs = Counter(f"{p['sym_a']}/{p['sym_b']}@{p['fee_rate']}" for p in pools)
    for k, v in pairs.most_common(40):
        print(" ", k, v)


if __name__ == "__main__":
    main()
