"""Read-only Turbos CLMM authoritative quoter via sui_devInspectTransactionBlock.
NEVER submits a tx; no signing; no state mutation (dev-inspect only).

pool::compute_swap_result is `friend` (uncallable from a PTB) — the public
authoritative path is the fetcher:
  pool_fetcher::compute_swap_result<A,B,Fee>(
     pool:&mut Pool, a_to_b:bool, amount:u128, by_amount_in:bool,
     sqrt_price_limit:u128, clock:&Clock, versioned:&Versioned, ctx)
  -> ComputeSwapState { amount_a, amount_b, amount_specified_remaining,
                        amount_calculated, sqrt_price, ... }  (all u128)
The output is `amount_calculated` (BCS offset 48 = 3 x u128).
"""

import base64

import cetus_rpc as c

# NOTE: Turbos is upgraded; calls must use the LATEST package id (the original
# 0x91bfbc... aborts in check_version). Pools/types still reference the origin pkg.
TURBOS_PKG = "0xa5a0c25c79e428eba04fb98b3fb2a34db45ab26d4c8faf0d7e39d66a63891e64"
TURBOS_ORIGIN_PKG = "0x91bfbc386a41afcfd9b2533058d7e915a1d3829089cc268ff4333d54d6339ca1"
VERSIONED_ID = "0xf1cf0e81048df168ebeb1b8030fad24b3e0b53ae827c25053fff0779c1445b6f"
VERSIONED_ISV = 1621135
CLOCK_ID = "0x6"
CLOCK_ISV = 1
MIN_SQRT = 4295048016
MAX_SQRT = 79226673515401279992447579055


def _mc(module, func, tt, argidxs):
    return (bytes([0]) + c.addr32(TURBOS_PKG) + c.ident(module) + c.ident(func)
            + c.vec(tt) + c.vec([c.arg_input(i) for i in argidxs]))


def _txkind(inputs, cmds):
    return base64.b64encode(bytes([0]) + c.vec(inputs) + c.vec(cmds)).decode()


def _devinspect(txb):
    res = c.rpc("sui_devInspectTransactionBlock", [c.SENDER, txb, None, None])
    if res.get("error"):
        raise RuntimeError(res["error"])
    return res["results"]


def _quote(pool_id, isv, ta, tb, fee_type, a_to_b, amount, by_amount_in):
    limit = MIN_SQRT if a_to_b else MAX_SQRT
    inputs = [
        c.shared_obj(pool_id, isv, True),                 # &mut Pool
        c.pure(bytes([1 if a_to_b else 0])),              # a_to_b
        c.pure(int(amount).to_bytes(16, "little")),       # amount: u128
        c.pure(bytes([1 if by_amount_in else 0])),        # by_amount_in
        c.pure(limit.to_bytes(16, "little")),             # sqrt_price_limit: u128
        c.shared_obj(CLOCK_ID, CLOCK_ISV, False),         # &Clock
        c.shared_obj(VERSIONED_ID, VERSIONED_ISV, False),  # &Versioned
    ]
    tt = [c.type_tag(ta), c.type_tag(tb), c.type_tag(fee_type)]
    cmds = [_mc("pool_fetcher", "compute_swap_result", tt, [0, 1, 2, 3, 4, 5, 6])]
    rv = _devinspect(_txkind(inputs, cmds))[0]["returnValues"][0][0]
    b = bytes(rv)
    # ComputeSwapState: amount_a(16) amount_b(16) amount_specified_remaining(16) amount_calculated(16)
    return int.from_bytes(b[48:64], "little")


def quote_exact_in(pool_id, isv, ta, tb, fee_type, a_to_b, amount_in):
    """Authoritative exact-input quote. Returns amount_out (Turbos' own pricing)."""
    return _quote(pool_id, isv, ta, tb, fee_type, a_to_b, amount_in, True)


def quote_full(pool_id, isv, ta, tb, fee_type, a_to_b, amount_in):
    """Authoritative exact-in quote with end state (for tick-crossing classification).
    ComputeSwapState: amount_a,amount_b,amount_specified_remaining,amount_calculated,
    sqrt_price (5 u128 = 80 bytes), then tick_current_index (i32 {bits:u32} = 4 bytes)."""
    limit = MIN_SQRT if a_to_b else MAX_SQRT
    inputs = [
        c.shared_obj(pool_id, isv, True),
        c.pure(bytes([1 if a_to_b else 0])),
        c.pure(int(amount_in).to_bytes(16, "little")),
        c.pure(bytes([1])),
        c.pure(limit.to_bytes(16, "little")),
        c.shared_obj(CLOCK_ID, CLOCK_ISV, False),
        c.shared_obj(VERSIONED_ID, VERSIONED_ISV, False),
    ]
    tt = [c.type_tag(ta), c.type_tag(tb), c.type_tag(fee_type)]
    cmds = [_mc("pool_fetcher", "compute_swap_result", tt, [0, 1, 2, 3, 4, 5, 6])]
    rv = bytes(_devinspect(_txkind(inputs, cmds))[0]["returnValues"][0][0])
    out = int.from_bytes(rv[48:64], "little")
    end_sqrt = int.from_bytes(rv[64:80], "little")
    bits = int.from_bytes(rv[80:84], "little")
    end_tick = bits - 2 ** 32 if bits >= 2 ** 31 else bits
    return {"amount_out": out, "end_sqrt_price": end_sqrt, "end_tick": end_tick}


def quote_exact_out(pool_id, isv, ta, tb, fee_type, a_to_b, amount_out):
    """Authoritative exact-output quote. Returns amount_in required."""
    return _quote(pool_id, isv, ta, tb, fee_type, a_to_b, amount_out, False)


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
    tick = f.get("tick_current_index", {})
    tick = int(tick["fields"]["bits"]) if isinstance(tick, dict) and "fields" in tick else None
    if tick is not None and tick >= 2 ** 31:
        tick -= 2 ** 32
    return {
        "id": pool_id,
        "isv": int(d["owner"]["Shared"]["initial_shared_version"]),
        "type_a": parts[0], "type_b": parts[1], "fee_type": parts[2],
        "sqrt_price": int(f["sqrt_price"]),
        "liquidity": int(f["liquidity"]),
        "fee": int(f["fee"]),
        "tick_spacing": int(f["tick_spacing"]),
        "tick": tick,
        "unlocked": bool(f.get("unlocked", True)),
    }


def page_pool_ids(pages=10, per=50):
    ids, cursor = [], None
    for _ in range(pages):
        res = c.rpc("suix_queryEvents",
                    [{"MoveEventModule": {"package": TURBOS_ORIGIN_PKG, "module": "pool"}}, cursor, per, True])
        for e in res.get("data", []):
            pid = e.get("parsedJson", {}).get("pool")
            if pid and pid not in ids:
                ids.append(pid)
        cursor = res.get("nextCursor")
        if not res.get("hasNextPage"):
            break
    return ids


if __name__ == "__main__":
    pid = "0x0df4f02d0e210169cb6d5aabd03c3058328c06f2c4dbb0804faa041159c78443"
    p = get_pool(pid)
    print("pool:", p["type_a"].split("::")[-1], "/", p["type_b"].split("::")[-1],
          "fee_type=", p["fee_type"].split("::")[-1], "fee=", p["fee"], "L=", p["liquidity"])
    for amt in (1_000_000_000, 100_000_000_000):
        out = quote_exact_in(pid, p["isv"], p["type_a"], p["type_b"], p["fee_type"], True, amt)
        print(f"  a_to_b in={amt} -> amount_calculated={out}")
    print("discover:", len(page_pool_ids()), "pool ids")
