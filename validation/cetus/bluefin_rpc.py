"""Read-only Bluefin Spot CLMM access: authoritative swap quotes + pool state via
sui_devInspectTransactionBlock. NEVER submits a tx.

Bluefin is a Uniswap-V3-style CLMM. Authoritative quote =
  pool::calculate_swap_results<X,Y>(pool, a2b, by_amount_in, amount:u64, sqrt_price_limit:u128)
    -> SwapResult { ..., amount_calculated, ... }
amount_calculated (the output) is at BCS offset 18 (a2b+by_amount_in+amount_specified
+amount_specified_remaining = 1+1+8+8).
"""

import base64

import cetus_rpc as c

BLUEFIN_PKG = "0x3492c874c1e3b3e2984e8c41b589e642d4d0a5d6459e5a9cfc2d52fd7c89c267"
# X64 sqrt-price bounds (same family as Cetus/Turbos/Momentum)
MIN_SQRT = 4295048016
MAX_SQRT = 79226673515401279992447579055


def _mc(module, func, tt, argidxs):
    return (bytes([0]) + c.addr32(BLUEFIN_PKG) + c.ident(module) + c.ident(func)
            + c.vec(tt) + c.vec([c.arg_input(i) for i in argidxs]))


def _txkind(inputs, cmds):
    return base64.b64encode(bytes([0]) + c.vec(inputs) + c.vec(cmds)).decode()


def _devinspect(txb):
    res = c.rpc("sui_devInspectTransactionBlock", [c.SENDER, txb, None, None])
    if res.get("error"):
        raise RuntimeError(res["error"])
    return res["results"]


def quote(pool_id, isv, type_x, type_y, a2b, amount):
    """Authoritative exact-in quote. a2b=True swaps X->Y. Returns amount_out."""
    limit = MIN_SQRT if a2b else MAX_SQRT
    inputs = [
        c.shared_obj(pool_id, isv, False),
        c.pure(bytes([1 if a2b else 0])),
        c.pure(bytes([1])),                       # by_amount_in
        c.pure(int(amount).to_bytes(8, "little")),  # amount: u64
        c.pure(limit.to_bytes(16, "little")),     # sqrt_price_limit: u128
    ]
    tt = [c.type_tag(type_x), c.type_tag(type_y)]
    cmds = [_mc("pool", "calculate_swap_results", tt, [0, 1, 2, 3, 4])]
    rv = _devinspect(_txkind(inputs, cmds))[0]["returnValues"][0][0]
    b = bytes(rv)
    # SwapResult: a2b(1) by_amount_in(1) amount_specified(8) amount_specified_remaining(8) amount_calculated(8)
    return int.from_bytes(b[18:26], "little")


def get_pool(pool_id):
    d = c.rpc("sui_getObject", [pool_id, {"showType": True, "showContent": True, "showOwner": True}])["data"]
    t = d["type"]
    inside = t.split("Pool<", 1)[1].rsplit(">", 1)[0]
    parts, depth, cur = [], 0, ""
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
    f = d["content"]["fields"]
    return {
        "id": pool_id,
        "isv": int(d["owner"]["Shared"]["initial_shared_version"]),
        "type_x": parts[0], "type_y": parts[1],
        "sqrt_price": int(f["current_sqrt_price"]),
        "liquidity": int(f["liquidity"]),
        "fee_rate": int(f["fee_rate"]),
        "is_paused": bool(f.get("is_paused", False)),
    }


def page_pool_ids(pages=10, per=50):
    ids, cursor = [], None
    for _ in range(pages):
        for mod in ("events",):  # SwapEvent is defined in the events module
            res = c.rpc("suix_queryEvents",
                        [{"MoveEventModule": {"package": BLUEFIN_PKG, "module": mod}}, cursor, per, True])
            for e in res.get("data", []):
                pj = e.get("parsedJson", {})
                pid = pj.get("pool_id") or pj.get("pool")
                if pid and pid not in ids:
                    ids.append(pid)
            cursor = res.get("nextCursor")
            if not res.get("hasNextPage"):
                return ids
    return ids


if __name__ == "__main__":
    pid = "0xcd8294c7507df2c5b21e065067d1e36ddbea41f273425019bd9f9935bce40b58"
    p = get_pool(pid)
    print("pool:", p["type_x"].split("::")[-1], "/", p["type_y"].split("::")[-1],
          "fee_rate=", p["fee_rate"], "L=", p["liquidity"], "paused=", p["is_paused"])
    for amt in (1_000_000_000, 100_000_000_000):
        out = quote(pid, p["isv"], p["type_x"], p["type_y"], True, amt)
        print(f"  SUI->USDC in={amt/1e9} SUI -> {out} ({out/1e6} USDC)")
    print("discover:", len(page_pool_ids()), "pool ids")
