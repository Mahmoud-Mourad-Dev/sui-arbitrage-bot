"""Read-only multi-venue arbitrage opportunity scanner for Sui mainnet.

Venues (all read-only via JSON-RPC; NO transactions, NO swaps):
  - Cetus  CLMM  (sqrt_price X64, fee_rate pips)   [pricing parity-certified]
  - Turbos CLMM  (sqrt_price X64, fee pips)         [same X64 math, not certified]
  - Kriya  AMM   (constant-product reserves, lp+protocol fee pips)

Builds a fee-adjusted directed token graph from spot prices and searches for
profitable cycles (2-hop cross-venue, 3-hop, 4-hop). The zero-slippage,
fee-adjusted cycle edge is the sound existence signal; realizable net profit is
estimated with a depth-based slippage haircut minus gas.

Subcommands: discover | sanity | scan <minutes> | analyze
"""

import json
import sys
import time
from collections import defaultdict

import cetus_rpc as c

RPC = c.MAINNET_RPC

VENUES = {
    "cetus": {"pkg": "0x1eabed72c53feb3805120a081dc15963c204dc8d091542592abaf7a35689b2fb",
              "module": "pool", "kind": "clmm"},
    "turbos": {"pkg": "0x91bfbc386a41afcfd9b2533058d7e915a1d3829089cc268ff4333d54d6339ca1",
               "module": "pool", "kind": "clmm"},
    "kriya": {"pkg": "0xa0eba10b173538c8fecca1dff298e488402cc9ff374f8a12ca7758eebe830b66",
              "module": "spot_dex", "kind": "amm"},
    "momentum": {"pkg": "0x70285592c97965e811e0c6f98dccc3a9c2b4ad854b3594faab9597ada267b860",
                 "module": "trade", "kind": "clmm"},
}

_dec_cache = {}


def coin_decimals(coin_type):
    if coin_type in _dec_cache:
        return _dec_cache[coin_type]
    try:
        md = c.rpc("suix_getCoinMetadata", [coin_type])
        d = int(md["decimals"]) if md else None
    except Exception:
        d = None
    _dec_cache[coin_type] = d
    return d


def page_pool_ids(venue, pages=6, per=50):
    v = VENUES[venue]
    ids, cursor = [], None
    for _ in range(pages):
        params = [{"MoveEventModule": {"package": v["pkg"], "module": v["module"]}}, cursor, per, True]
        try:
            res = c.rpc("suix_queryEvents", params)
        except Exception:
            break
        for e in res.get("data", []):
            pj = e.get("parsedJson", {})
            pid = pj.get("pool") or pj.get("pool_id")
            if pid and pid not in ids:
                ids.append(pid)
        cursor = res.get("nextCursor")
        if not res.get("hasNextPage"):
            break
    return ids


def split_type_args(t):
    inside = t.split("<", 1)[1].rsplit(">", 1)[0]
    depth, parts, cur = 0, [], ""
    for ch in inside:
        if ch == "<":
            depth += 1
        if ch == ">":
            depth -= 1
        if ch == "," and depth == 0:
            parts.append(cur.strip()); cur = ""
        else:
            cur += ch
    parts.append(cur.strip())
    return parts


def parse_pool(venue, o):
    d = o.get("data")
    if not d:
        return None
    kind = VENUES[venue]["kind"]
    t = d["type"]
    f = d["content"]["fields"]
    targs = split_type_args(t)
    ta, tb = targs[0], targs[1]
    isv = None
    owner = d.get("owner")
    if isinstance(owner, dict) and "Shared" in owner:
        isv = int(owner["Shared"]["initial_shared_version"])
    rec = {"venue": venue, "id": d["objectId"], "type_a": ta, "type_b": tb, "isv": isv,
           "sym_a": ta.split("::")[-1], "sym_b": tb.split("::")[-1]}
    if kind == "clmm":
        # Turbos: skip locked pools; Cetus: skip paused pools.
        if f.get("unlocked") is False or f.get("is_pause") is True:
            return None
        dec_a, dec_b = coin_decimals(ta), coin_decimals(tb)
        if dec_a is None or dec_b is None:
            return None
        sp = int(f.get("current_sqrt_price") or f.get("sqrt_price"))
        s = sp / 2 ** 64
        price_b_per_a = s * s * 10 ** (dec_a - dec_b)  # whole token_b per whole token_a
        fee = int(f.get("fee_rate") or f.get("fee") or f.get("swap_fee_rate")) / 1_000_000.0
        liq = int(f["liquidity"])
        rec.update(dec_a=dec_a, dec_b=dec_b, price=price_b_per_a, fee=fee, depth=liq, kind="clmm")
    else:  # kriya amm
        # Only constant-product, swap-enabled pools. Stableswap pools use a
        # different invariant we do not model here -> exclude to avoid mispricing.
        if not f.get("is_swap_enabled", True) or f.get("is_stable", False):
            return None
        rx, ry = int(f["token_x"]), int(f["token_y"])
        if rx == 0 or ry == 0:
            return None
        sx, sy = int(f["scaleX"]), int(f["scaleY"])
        dec_a = len(str(sx)) - 1
        dec_b = len(str(sy)) - 1
        price_b_per_a = (ry / rx) * (sx / sy)
        fee = (int(f["lp_fee_percent"]) + int(f["protocol_fee_percent"])) / 1_000_000.0
        rec.update(dec_a=dec_a, dec_b=dec_b, price=price_b_per_a, fee=fee,
                   reserve_x=rx, reserve_y=ry, depth=min(rx, ry), kind="amm")
    return rec


def read_pools(ids):
    out = []
    for i in range(0, len(ids), 50):
        chunk = ids[i:i + 50]
        # group by venue not needed; we pass venue per call site
        out.append(chunk)
    return out


def discover():
    pools = []
    for venue in VENUES:
        ids = page_pool_ids(venue)
        print(f"{venue}: {len(ids)} pool ids")
        for i in range(0, len(ids), 50):
            chunk = ids[i:i + 50]
            metas = c.rpc("sui_multiGetObjects", [chunk, {"showType": True, "showContent": True, "showOwner": True}])
            for o in metas:
                try:
                    r = parse_pool(venue, o)
                    if r and r["price"] > 0:
                        pools.append(r)
                except Exception:
                    pass
    json.dump(pools, open("mv_pools.json", "w"))
    by_v = defaultdict(int)
    for p in pools:
        by_v[p["venue"]] += 1
    print("parsed pools:", dict(by_v), "total", len(pools))
    return pools


def sanity():
    pools = json.load(open("mv_pools.json"))
    # cross-venue mid-price for SUI/USDC-ish pairs
    print("Cross-venue mid-price check (whole token_b per token_a):")
    for sym in ("USDC", "USDT", "DEEP", "CETUS", "WAL"):
        rows = [p for p in pools if {p["sym_a"], p["sym_b"]} == {sym, "SUI"}]
        if len(rows) < 2:
            continue
        print(f"  {sym}/SUI:")
        for p in rows:
            # normalize to SUI per <sym>
            if p["sym_a"] == "SUI":
                price = 1.0 / p["price"]  # <sym> per SUI -> invert to SUI per sym
            else:
                price = p["price"]
            print(f"    {p['venue']:7} fee={p['fee']*100:.3f}%  SUI per {sym} = {price:.6g}")


USD_SYMS = {"USDC", "USDT", "BUCK", "USDB", "AUSD", "FDUSD", "USDY", "WUSDC"}
HUB_SYMS = {"SUI"} | USD_SYMS
# gas model (SUI): base + per-hop, converted to USD at scan time.
GAS_SUI_BASE = 0.003
GAS_SUI_PER_HOP = 0.0012
INPUT_USD_GRID = [1, 5, 10, 50, 100, 500, 1000, 5000, 10000]


def cp_out(amt, rin, rout, fee):
    a = amt * (1.0 - fee)
    return a * rout / (rin + a)


def virtual_reserves(p):
    """Native in-range reserves for slippage sim: AMM uses real reserves, CLMM uses
    virtual reserves x=L*2^64/sqrtP (token0), y=L*sqrtP/2^64 (token1)."""
    if p["kind"] == "amm":
        return p["reserve_x"], p["reserve_y"]
    # need sqrt_price + L; recompute sqrt from price/decimals is lossy, so store at read
    sp = p["_sqrt"]
    L = p["depth"]
    x = L * (2 ** 64) // sp
    y = L * sp // (2 ** 64)
    return x, y


def build_edges(pools):
    edges = {}  # (from,to) -> best edge dict
    for p in pools:
        rx, ry = virtual_reserves(p)
        if rx <= 0 or ry <= 0:
            continue
        for (frm, to, rin, rout, dfrm, dto) in (
            (p["type_a"], p["type_b"], rx, ry, p["dec_a"], p["dec_b"]),
            (p["type_b"], p["type_a"], ry, rx, p["dec_b"], p["dec_a"]),
        ):
            mid = (rout / rin) * (1 - p["fee"])
            e = edges.get((frm, to))
            if e is None or mid > e["mid"]:
                edges[(frm, to)] = {"from": frm, "to": to, "rin": rin, "rout": rout,
                                    "fee": p["fee"], "mid": mid, "venue": p["venue"],
                                    "pool": p["id"], "dec_from": dfrm, "dec_to": dto,
                                    "sym_from": frm.split("::")[-1], "sym_to": to.split("::")[-1],
                                    "isv": p["isv"], "type_a": p["type_a"], "type_b": p["type_b"],
                                    "a2b": (frm == p["type_a"]), "kind": p["kind"]}
    return edges


def adjacency(edges):
    adj = defaultdict(list)
    for (frm, _to), e in edges.items():
        adj[frm].append(e)
    return adj


def find_cycles(adj, base, max_hops):
    cycles = []

    def dfs(node, path, visited):
        for e in adj.get(node, []):
            nlen = len(path) + 1
            if e["to"] == base:
                if 2 <= nlen <= max_hops:
                    cycles.append(path + [e])
                continue
            if e["to"] not in visited and nlen < max_hops:
                visited.add(e["to"])
                dfs(e["to"], path + [e], visited)
                visited.remove(e["to"])

    dfs(base, [], {base})
    return cycles


def zero_slip_edge(cyc):
    prod = 1.0
    for e in cyc:
        prod *= e["mid"]
    return prod - 1.0


def best_net(cyc, base_dec, usd_base, gas_usd):
    best = None
    for usd in INPUT_USD_GRID:
        amt = usd / usd_base * 10 ** base_dec
        x = amt
        for e in cyc:
            x = cp_out(x, e["rin"], e["rout"], e["fee"])
        gross_native = x - amt
        gross_usd = gross_native / 10 ** base_dec * usd_base
        net = gross_usd - gas_usd
        if best is None or net > best["net_usd"]:
            best = {"input_usd": usd, "gross_usd": gross_usd, "gas_usd": gas_usd, "net_usd": net}
    return best


def refresh(id_venue):
    ids = list(id_venue.keys())
    pools = []
    for i in range(0, len(ids), 50):
        chunk = ids[i:i + 50]
        metas = c.rpc("sui_multiGetObjects", [chunk, {"showType": True, "showContent": True, "showOwner": True}])
        for o in metas:
            try:
                v = id_venue[o["data"]["objectId"]]
                r = parse_pool(v, o)
                if not r or r["price"] <= 0:
                    continue
                if r["kind"] == "clmm":
                    f = o["data"]["content"]["fields"]
                    r["_sqrt"] = int(f.get("current_sqrt_price") or f.get("sqrt_price"))
                pools.append(r)
            except Exception:
                pass
    return pools


def sui_usd_from(pools):
    for p in pools:
        if {p["sym_a"], p["sym_b"]} == {"USDC", "SUI"}:
            if p["sym_a"] == "USDC":
                return 1.0 / p["price"]  # SUI per USDC inverted -> USD per SUI
            return p["price"]
    return None


def usd_of(sym, sui_usd):
    if sym in USD_SYMS:
        return 1.0
    if sym == "SUI":
        return sui_usd
    return None


def scan(minutes):
    base_pools = json.load(open("mv_pools.json"))
    id_venue = {p["id"]: p["venue"] for p in base_pools}
    deadline = time.time() + minutes * 60
    rnd = 0
    logf = open("mv_opps.jsonl", "a")
    while time.time() < deadline:
        rnd += 1
        try:
            pools = refresh(id_venue)
        except Exception as e:
            print("refresh error", e)
            time.sleep(10)
            continue
        sui_usd = sui_usd_from(pools)
        if not sui_usd:
            time.sleep(10)
            continue
        edges = build_edges(pools)
        adj = adjacency(edges)
        # hub base tokens present
        bases = {}
        for p in pools:
            for sym, ct, dec in ((p["sym_a"], p["type_a"], p["dec_a"]), (p["sym_b"], p["type_b"], p["dec_b"])):
                if sym in HUB_SYMS and ct not in bases:
                    bases[ct] = (sym, dec)
        found = 0
        for base_ct, (bsym, bdec) in bases.items():
            ub = usd_of(bsym, sui_usd)
            if not ub:
                continue
            for cyc in find_cycles(adj, base_ct, 4):
                edge = zero_slip_edge(cyc)
                if edge <= 0:
                    continue  # no dislocation beyond fees
                hops = len(cyc)
                gas_usd = (GAS_SUI_BASE + GAS_SUI_PER_HOP * hops) * sui_usd
                bn = best_net(cyc, bdec, ub, gas_usd)
                rec = {
                    "ts": time.time(), "round": rnd, "base": bsym, "hops": hops,
                    "edge_bps": round(edge * 10000, 3),
                    "path": [e["sym_from"] for e in cyc] + [bsym],
                    "venues": [e["venue"] for e in cyc],
                    "input_usd": bn["input_usd"], "gross_usd": round(bn["gross_usd"], 6),
                    "gas_usd": round(bn["gas_usd"], 6), "net_usd": round(bn["net_usd"], 6),
                }
                logf.write(json.dumps(rec) + "\n")
                found += 1
        logf.flush()
        print(f"round {rnd} t+{int(time.time()-(deadline-minutes*60))}s SUI=${sui_usd:.4f} "
              f"edges={len(edges)} dislocations={found}")
        time.sleep(20)
    logf.close()
    print("scan complete, rounds:", rnd)


def analyze():
    import statistics
    rows = [json.loads(l) for l in open("mv_opps.jsonl")]
    print("total dislocation records (edge>0):", len(rows))
    prof = [r for r in rows if r["net_usd"] > 0]
    print("profitable after gas (net>0):", len(prof))
    if rows:
        ts = [r["ts"] for r in rows]
        span_h = (max(ts) - min(ts)) / 3600 or 1e-9
        print(f"window: {span_h:.3f} h  | dislocations/hour: {len(rows)/span_h:.2f}  | profitable/hour: {len(prof)/span_h:.2f}")
    if prof:
        nets = sorted(r["net_usd"] for r in prof)
        print(f"net profit ($): mean={statistics.mean(nets):.4f} median={statistics.median(nets):.4f} "
              f"p95={nets[min(len(nets)-1,int(len(nets)*0.95))]:.4f} max={max(nets):.4f} min={min(nets):.4f}")


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "discover"
    if cmd == "discover":
        discover()
    elif cmd == "sanity":
        sanity()
    elif cmd == "scan":
        scan(float(sys.argv[2]) if len(sys.argv) > 2 else 30)
    elif cmd == "analyze":
        analyze()
