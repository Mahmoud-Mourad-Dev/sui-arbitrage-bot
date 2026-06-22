"""Read-only Momentum (mmt.finance) CLMM access: authoritative swap quotes +
pool/state discovery via sui_devInspectTransactionBlock. NEVER submits a tx.

Momentum is a Uniswap-V3-style CLMM. Authoritative quote =
  trade::compute_swap_result<X,Y>(pool, x_for_y, by_amount_in, sqrt_price_limit:u128, amount:u64) -> SwapState
and we read SwapState.amount_calculated (the output). Same dev-inspect pattern as
Cetus calculate_swap_result.
"""

import base64

import cetus_rpc as c

MMT_PKG = "0x70285592c97965e811e0c6f98dccc3a9c2b4ad854b3594faab9597ada267b860"


def _mc(module, func, tt, args):
    return bytes([0]) + c.addr32(MMT_PKG) + c.ident(module) + c.ident(func) + c.vec(tt) + c.vec(args)


def _txkind(inputs, cmds):
    return base64.b64encode(bytes([0]) + c.vec(inputs) + c.vec(cmds)).decode()


def _devinspect(txb):
    res = c.rpc("sui_devInspectTransactionBlock", [c.SENDER, txb, None, None])
    if res.get("error"):
        raise RuntimeError(res["error"])
    return res["results"]


def sqrt_bound(which):  # "min_sqrt_price" / "max_sqrt_price" -> u128
    rv = _devinspect(_txkind([], [_mc("tick_math", which, [], [])]))[0]["returnValues"][0][0]
    return int.from_bytes(bytes(rv), "little")


_BOUNDS = {}


def bounds():
    if not _BOUNDS:
        _BOUNDS["min"] = sqrt_bound("min_sqrt_price")
        _BOUNDS["max"] = sqrt_bound("max_sqrt_price")
    return _BOUNDS["min"], _BOUNDS["max"]


def quote(pool_id, isv, type_x, type_y, x_for_y, amount):
    """Authoritative exact-in quote. x_for_y=True swaps X->Y. Returns amount_out (Y or X)."""
    lo, hi = bounds()
    limit = lo if x_for_y else hi
    inputs = [
        c.shared_obj(pool_id, isv, False),
        c.pure(bytes([1 if x_for_y else 0])),   # x_for_y
        c.pure(bytes([1])),                      # by_amount_in
        c.pure(limit.to_bytes(16, "little")),    # sqrt_price_limit: u128
        c.pure(int(amount).to_bytes(8, "little")),  # amount: u64
    ]
    tt = [c.type_tag(type_x), c.type_tag(type_y)]
    cmds = [_mc("trade", "compute_swap_result", tt,
                [c.arg_input(0), c.arg_input(1), c.arg_input(2), c.arg_input(3), c.arg_input(4)])]
    rv = _devinspect(_txkind(inputs, cmds))[0]["returnValues"][0][0]
    b = bytes(rv)
    # SwapState: amount_specified_remaining u64, amount_calculated u64, ...
    return int.from_bytes(b[8:16], "little")


def get_pool(pool_id):
    d = c.rpc("sui_getObject", [pool_id, {"showType": True, "showContent": True, "showOwner": True}])["data"]
    t = d["type"]
    inside = t.split("Pool<", 1)[1].rsplit(">", 1)[0] if "Pool<" in t else ""
    f = d["content"]["fields"]
    parts = []
    depth = 0
    cur = ""
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
    return {
        "id": pool_id,
        "isv": int(d["owner"]["Shared"]["initial_shared_version"]),
        "type_x": parts[0] if parts and parts[0] else f.get("type_x"),
        "type_y": parts[1] if len(parts) > 1 else f.get("type_y"),
        "sqrt_price": int(f["sqrt_price"]),
        "liquidity": int(f["liquidity"]),
        "fee_rate": int(f["swap_fee_rate"]),
        "tick_spacing": int(f["tick_spacing"]),
        "reserve_x": int(f.get("reserve_x", 0)),
        "reserve_y": int(f.get("reserve_y", 0)),
    }


def page_pool_ids(pages=8, per=50):
    ids, cursor = [], None
    for _ in range(pages):
        res = c.rpc("suix_queryEvents",
                    [{"MoveEventModule": {"package": MMT_PKG, "module": "trade"}}, cursor, per, True])
        for e in res.get("data", []):
            pid = e.get("parsedJson", {}).get("pool_id")
            if pid and pid not in ids:
                ids.append(pid)
        cursor = res.get("nextCursor")
        if not res.get("hasNextPage"):
            break
    return ids


if __name__ == "__main__":
    print("sqrt bounds:", bounds())
    pid = "0xc7993ebf7a1e629a942f69e9b3a8dccc3a96db0df3991812578815a0dda08a91"
    p = get_pool(pid)
    print("pool:", p["type_x"].split("::")[-1], "/", p["type_y"].split("::")[-1],
          "fee_rate=", p["fee_rate"], "spacing=", p["tick_spacing"], "L=", p["liquidity"])
    for amt in (1_000_000, 1_000_000_000):
        out = quote(pid, p["isv"], p["type_x"], p["type_y"], True, amt)
        print(f"  x_for_y in={amt} -> amount_calculated={out}")
