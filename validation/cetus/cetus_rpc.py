"""Read-only Cetus access: pool/tick state decoding + authoritative quotes via
sui_devInspectTransactionBlock. NEVER submits a transaction.

The quote path hand-builds the BCS of a `TransactionKind::ProgrammableTransaction`
containing a single `pool::calculate_swap_result` move-call, then dev-inspects it.
calculate_swap_result is a pure read on pool state, so dev-inspect returns the
authoritative Cetus quote with no gas and no signing.
"""

import base64
import json
import urllib.request

CLMM_PKG = "0x1eabed72c53feb3805120a081dc15963c204dc8d091542592abaf7a35689b2fb"
MAINNET_RPC = "https://fullnode.mainnet.sui.io:443"
# Any valid address works as the dev-inspect sender (no gas needed).
SENDER = "0x0000000000000000000000000000000000000000000000000000000000000001"


def rpc(method, params, url=MAINNET_RPC):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    req = urllib.request.Request(url, data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=40) as r:
        d = json.loads(r.read())
    if "error" in d:
        raise RuntimeError(f"{method} error: {d['error']}")
    return d["result"]


# --- BCS encoding (only what the quoter PTB needs) ------------------------

def uleb(n):
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def addr32(h):
    h = h[2:] if h.startswith("0x") else h
    return bytes.fromhex(h.rjust(64, "0"))


def ident(s):
    b = s.encode()
    return uleb(len(b)) + b


def vec(items):
    return uleb(len(items)) + b"".join(items)


def type_tag(coin):
    """TypeTag for a simple `addr::module::Name` coin type (no generics)."""
    a, m, n = coin.split("::")
    # TypeTag enum: Struct = 7; StructTag = addr ++ module ++ name ++ vec<TypeTag>
    return bytes([7]) + addr32(a) + ident(m) + ident(n) + vec([])


def pure(payload):
    # CallArg::Pure(vec<u8>)
    return bytes([0]) + uleb(len(payload)) + payload


def shared_obj(obj_id, init_version, mutable):
    # CallArg::Object(ObjectArg::SharedObject{ id, initial_shared_version, mutable })
    return bytes([1, 1]) + addr32(obj_id) + init_version.to_bytes(8, "little") + bytes([1 if mutable else 0])


def arg_input(i):
    # Argument::Input(u16)
    return bytes([1]) + i.to_bytes(2, "little")


def build_quote_txkind(pool_id, init_version, type_a, type_b, a2b, by_amount_in, amount):
    inputs = [
        shared_obj(pool_id, init_version, False),
        pure(bytes([1 if a2b else 0])),
        pure(bytes([1 if by_amount_in else 0])),
        pure(amount.to_bytes(8, "little")),
    ]
    move_call = (
        bytes([0])  # Command::MoveCall
        + addr32(CLMM_PKG)
        + ident("pool")
        + ident("calculate_swap_result")
        + vec([type_tag(type_a), type_tag(type_b)])
        + vec([arg_input(0), arg_input(1), arg_input(2), arg_input(3)])
    )
    ptb = vec(inputs) + vec([move_call])
    txkind = bytes([0]) + ptb  # TransactionKind::ProgrammableTransaction
    return base64.b64encode(txkind).decode()


# --- decode CalculatedSwapResult ------------------------------------------

def decode_swap_result(byte_list):
    """Decode the BCS of CalculatedSwapResult.
    Layout: amount_in u64, amount_out u64, fee_amount u64, fee_rate u64,
            after_sqrt_price u128, is_exceed bool, step_results vec<...>."""
    b = bytes(byte_list)
    le = lambda lo, hi: int.from_bytes(b[lo:hi], "little")
    return {
        "amount_in": le(0, 8),
        "amount_out": le(8, 16),
        "fee_amount": le(16, 24),
        "fee_rate": le(24, 32),
        "after_sqrt_price": le(32, 48),
        "is_exceed": b[48] != 0,
        # step count (number of swap steps == tick segments touched)
        "n_steps": _uleb_at(b, 49)[0],
    }


def _uleb_at(b, off):
    shift = 0
    val = 0
    while True:
        byte = b[off]
        off += 1
        val |= (byte & 0x7F) << shift
        if not (byte & 0x80):
            return val, off
        shift += 7


def cetus_quote(pool_id, init_version, type_a, type_b, a2b, amount, url=MAINNET_RPC):
    """Authoritative exact-in quote from Cetus (read-only)."""
    txb = build_quote_txkind(pool_id, init_version, type_a, type_b, a2b, True, amount)
    res = rpc("sui_devInspectTransactionBlock", [SENDER, txb, None, None], url)
    err = res.get("error")
    if err:
        raise RuntimeError(f"devInspect error: {err}")
    rv = res["results"][0]["returnValues"][0][0]
    return decode_swap_result(rv)


# --- ticks (for cross-tick / Stage B) -------------------------------------

def _i_from_bits(bits, width):
    return bits - (1 << width) if bits >= (1 << (width - 1)) else bits


def build_fetch_ticks_txkind(pool_id, init_version, type_a, type_b, start_payload, limit):
    inputs = [
        shared_obj(pool_id, init_version, False),
        pure(start_payload),                       # start: vector<u32>
        pure(limit.to_bytes(8, "little")),         # limit: u64
    ]
    move_call = (
        bytes([0])
        + addr32(CLMM_PKG)
        + ident("pool")
        + ident("fetch_ticks")
        + vec([type_tag(type_a), type_tag(type_b)])
        + vec([arg_input(0), arg_input(1), arg_input(2)])
    )
    ptb = vec(inputs) + vec([move_call])
    return base64.b64encode(bytes([0]) + ptb).decode()


def decode_ticks(byte_list):
    b = bytes(byte_list)
    count, off = _uleb_at(b, 0)
    ticks = []
    for _ in range(count):
        index_bits = int.from_bytes(b[off:off + 4], "little"); off += 4
        idx = _i_from_bits(index_bits, 32)
        sqrt_price = int.from_bytes(b[off:off + 16], "little"); off += 16
        net_bits = int.from_bytes(b[off:off + 16], "little"); off += 16
        net = _i_from_bits(net_bits, 128)
        off += 16          # liquidity_gross
        off += 16 * 3      # fee_growth_a, fee_growth_b, points_growth
        rcount, off = _uleb_at(b, off)
        off += 16 * rcount  # rewards_growth_outside: vector<u128>
        ticks.append({"index": idx, "sqrt_price": sqrt_price, "liquidity_net": net})
    return ticks


def fetch_all_ticks(pool_id, init_version, type_a, type_b, batch=512, max_batches=20, url=MAINNET_RPC):
    out = []
    start_payload = uleb(0)  # empty vector<u32> -> from the start
    for _ in range(max_batches):
        txb = build_fetch_ticks_txkind(pool_id, init_version, type_a, type_b, start_payload, batch)
        res = rpc("sui_devInspectTransactionBlock", [SENDER, txb, None, None], url)
        rv = res["results"][0]["returnValues"][0][0]
        ticks = decode_ticks(rv)
        out.extend(ticks)
        if len(ticks) < batch:
            break
        last = ticks[-1]["index"] & 0xFFFFFFFF
        start_payload = uleb(1) + last.to_bytes(4, "little")  # resume after last index
    return out


# --- pool state -----------------------------------------------------------

def get_pool_state(pool_id, url=MAINNET_RPC):
    d = rpc("sui_getObject", [pool_id, {"showType": True, "showContent": True, "showOwner": True}], url)
    data = d["data"]
    f = data["content"]["fields"]
    t = data["type"]
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
    tick = f["current_tick_index"]["fields"]["bits"]
    tick = int(tick)
    if tick >= 2**31:
        tick -= 2**32  # I32 stored as u32 bits
    return {
        "id": pool_id,
        "version": int(data["version"]),
        "type_a": parts[0],
        "type_b": parts[1],
        "sqrt_price": int(f["current_sqrt_price"]),
        "liquidity": int(f["liquidity"]),
        "fee_rate": int(f["fee_rate"]),
        "tick_spacing": int(f["tick_spacing"]),
        "current_tick": tick,
        "init_shared_version": int(data["owner"]["Shared"]["initial_shared_version"]),
        "coin_a_balance": int(f["coin_a"]),
        "coin_b_balance": int(f["coin_b"]),
    }


def _move_call(module, func, type_tags, arg_idxs):
    return (
        bytes([0])
        + addr32(CLMM_PKG)
        + ident(module)
        + ident(func)
        + vec(type_tags)
        + vec([arg_input(i) for i in arg_idxs])
    )


def snapshot_and_quotes(pool_id, init_version, type_a, type_b, scenarios, url=MAINNET_RPC):
    """ONE dev-inspect returning, from a single atomic state snapshot:
    current_sqrt_price, liquidity, fee_rate, then a CalculatedSwapResult per
    (a2b, amount) scenario. This guarantees the engine and Cetus see identical
    state without any version guard."""
    inputs = [shared_obj(pool_id, init_version, False), pure(bytes([1])), pure(bytes([0]))]
    for _a2b, amount in scenarios:
        inputs.append(pure(amount.to_bytes(8, "little")))
    tt = [type_tag(type_a), type_tag(type_b)]
    cmds = [
        _move_call("pool", "current_sqrt_price", tt, [0]),
        _move_call("pool", "liquidity", tt, [0]),
        _move_call("pool", "fee_rate", tt, [0]),
    ]
    for i, (a2b, _amount) in enumerate(scenarios):
        a2b_idx = 1 if a2b else 2
        cmds.append(_move_call("pool", "calculate_swap_result", tt, [0, a2b_idx, 1, 3 + i]))
    ptb = vec(inputs) + vec(cmds)
    txb = base64.b64encode(bytes([0]) + ptb).decode()

    res = rpc("sui_devInspectTransactionBlock", [SENDER, txb, None, None], url)
    if res.get("error"):
        raise RuntimeError(f"devInspect error: {res['error']}")
    results = res["results"]
    u = lambda i: int.from_bytes(bytes(results[i]["returnValues"][0][0]), "little")
    snap = {"sqrt_price": u(0), "liquidity": u(1), "fee_rate": u(2)}
    quotes = [decode_swap_result(results[3 + i]["returnValues"][0][0]) for i in range(len(scenarios))]
    return snap, quotes


def get_pool_version(pool_id, url=MAINNET_RPC):
    d = rpc("sui_getObject", [pool_id, {}], url)
    return int(d["data"]["version"])


if __name__ == "__main__":
    POOL = "0xe01243f37f712ef87e556afb9b1d03d0fae13f96d324ec912daffc339dfdcbd2"
    st = get_pool_state(POOL)
    print("pool state:", json.dumps({k: st[k] for k in ("type_a", "type_b", "sqrt_price", "liquidity", "fee_rate", "tick_spacing", "current_tick")}, indent=0))
    for amt in (1_000_000, 1_000_000_000):
        q = cetus_quote(POOL, st["init_shared_version"], st["type_a"], st["type_b"], True, amt)
        ok = q["amount_in"] == amt or q["is_exceed"]
        print(f"a2b in={amt}: amount_in={q['amount_in']} amount_out={q['amount_out']} "
              f"fee={q['fee_amount']} steps={q['n_steps']} exceed={q['is_exceed']} selфcheck={ok}")
