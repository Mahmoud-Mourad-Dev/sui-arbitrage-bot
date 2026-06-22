"""Read-only DeepBook v3 access: authoritative swap quotes + orderbook depth via
sui_devInspectTransactionBlock. NEVER submits a transaction.

DeepBook is a CLOB, not an AMM: quotes come from its own on-chain quoters
(pool::get_quote_quantity_out / get_base_quantity_out), which walk the live order
book. `deep_required` is the DEEP-token fee the taker would owe for that fill (0 on
input-fee pools); arbitrage must account for it as a cost when non-zero.
"""

import base64

import cetus_rpc as c

DEEPBOOK_PKG = "0x0e735f8c93a95722efd73521aca7a7652c0bb71ed1daf41b26dfd7d1ff71f748"
CLOCK_ID = "0x6"
CLOCK_ISV = 1

# from the DeepBook v3 SDK constants (verified on-chain)
POOLS = {
    "SUI_USDC": {"id": "0xe05dafb5133bcffb8d59f4e12465dc0e9faeaa05e3e342a08fe135800e3e4407",
                 "base": "0x2::sui::SUI",
                 "quote": "0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC"},
    "DEEP_SUI": {"id": "0xb663828d6217467c8a1838a03793da896cbe745b150ebd57d82f814ca579fc22",
                 "base": "0xdeeb7a4662eec9f2f3def03fb937a663dddaa2e215b8078a284d026b7946c270::deep::DEEP",
                 "quote": "0x2::sui::SUI"},
    "DEEP_USDC": {"id": "0xf948981b806057580f91622417534f491da5f61aeaf33d0ed8e69fd5691c95ce",
                  "base": "0xdeeb7a4662eec9f2f3def03fb937a663dddaa2e215b8078a284d026b7946c270::deep::DEEP",
                  "quote": "0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC"},
}


def _mc(module, func, tt, argidxs):
    return (bytes([0]) + c.addr32(DEEPBOOK_PKG) + c.ident(module) + c.ident(func)
            + c.vec(tt) + c.vec([c.arg_input(i) for i in argidxs]))


def _txkind(inputs, cmds):
    return base64.b64encode(bytes([0]) + c.vec(inputs) + c.vec(cmds)).decode()


def _devinspect(txb):
    res = c.rpc("sui_devInspectTransactionBlock", [c.SENDER, txb, None, None])
    if res.get("error"):
        raise RuntimeError(res["error"])
    return res["results"]


def get_isv(pool_id):
    d = c.rpc("sui_getObject", [pool_id, {"showOwner": True}])
    return int(d["data"]["owner"]["Shared"]["initial_shared_version"])


def quote_base_to_quote(pool_id, isv, base, quote, base_qty):
    """Sell `base_qty` base -> quote out (authoritative)."""
    inputs = [c.shared_obj(pool_id, isv, False), c.pure(base_qty.to_bytes(8, "little")),
              c.shared_obj(CLOCK_ID, CLOCK_ISV, False)]
    tt = [c.type_tag(base), c.type_tag(quote)]
    rv = _devinspect(_txkind(inputs, [_mc("pool", "get_quote_quantity_out", tt, [0, 1, 2])]))[0]["returnValues"]
    u = lambda i: int.from_bytes(bytes(rv[i][0]), "little")
    return {"base_out": u(0), "quote_out": u(1), "deep_required": u(2)}


def quote_quote_to_base(pool_id, isv, base, quote, quote_qty):
    """Spend `quote_qty` quote -> base out (authoritative)."""
    inputs = [c.shared_obj(pool_id, isv, False), c.pure(quote_qty.to_bytes(8, "little")),
              c.shared_obj(CLOCK_ID, CLOCK_ISV, False)]
    tt = [c.type_tag(base), c.type_tag(quote)]
    rv = _devinspect(_txkind(inputs, [_mc("pool", "get_base_quantity_out", tt, [0, 1, 2])]))[0]["returnValues"]
    u = lambda i: int.from_bytes(bytes(rv[i][0]), "little")
    return {"base_out": u(0), "quote_out": u(1), "deep_required": u(2)}


def mid_price(pool_id, isv, base, quote):
    inputs = [c.shared_obj(pool_id, isv, False), c.shared_obj(CLOCK_ID, CLOCK_ISV, False)]
    tt = [c.type_tag(base), c.type_tag(quote)]
    rv = _devinspect(_txkind(inputs, [_mc("pool", "mid_price", tt, [0, 1])]))[0]["returnValues"]
    return int.from_bytes(bytes(rv[0][0]), "little")


def _decode_u64_vec(b):
    b = bytes(b)
    n, off = c._uleb_at(b, 0)
    return [int.from_bytes(b[off + 8 * i:off + 8 * i + 8], "little") for i in range(n)]


def level2_from_mid(pool_id, isv, base, quote, ticks=10):
    """(bid_prices, bid_qtys, ask_prices, ask_qtys) around mid — for best bid/ask
    and a local depth-walk used in parity testing."""
    inputs = [c.shared_obj(pool_id, isv, False), c.pure(ticks.to_bytes(8, "little")),
              c.shared_obj(CLOCK_ID, CLOCK_ISV, False)]
    tt = [c.type_tag(base), c.type_tag(quote)]
    rv = _devinspect(_txkind(inputs, [_mc("pool", "get_level2_ticks_from_mid", tt, [0, 1, 2])]))[0]["returnValues"]
    return tuple(_decode_u64_vec(rv[i][0]) for i in range(4))


FLOAT_SCALING = 1_000_000_000  # DeepBook v3 price scaling: quote = base*price/1e9


def depth_walk_base_to_quote(level2, base_qty, scale=FLOAT_SCALING):
    """Local model: walk the bid side selling `base_qty` base. Returns
    (quote_out_native, covered) where covered=False if the fetched book ran out."""
    bid_prices, bid_qtys, _ap, _aq = level2
    remaining = base_qty
    quote = 0
    for p, q in zip(bid_prices, bid_qtys):
        take = min(remaining, q)
        quote += take * p // scale
        remaining -= take
        if remaining == 0:
            return quote, True
    return quote, False


if __name__ == "__main__":
    p = POOLS["SUI_USDC"]
    isv = get_isv(p["id"])
    print("SUI_USDC isv:", isv)
    print("mid_price (raw):", mid_price(p["id"], isv, p["base"], p["quote"]))
    for sui in (1_000_000_000, 100_000_000_000):
        q = quote_base_to_quote(p["id"], isv, p["base"], p["quote"], sui)
        print(f"sell {sui/1e9} SUI -> quote_out={q['quote_out']} ({q['quote_out']/1e6} USDC) deep_req={q['deep_required']}")
    bp, bq, ap, aq = level2_from_mid(p["id"], isv, p["base"], p["quote"], 3)
    print("best bid:", bp[0] if bp else None, "best ask:", ap[0] if ap else None)
