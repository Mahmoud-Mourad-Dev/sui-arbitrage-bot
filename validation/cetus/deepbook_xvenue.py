"""Cross-venue AMM <-> DeepBook arbitrage scanner (READ-ONLY, authoritative).

For each pair quoted on both an AMM (Cetus CLMM) and DeepBook, simulate the
round-trip base -> mid -> base in BOTH cross-venue directions, using each venue's
own authoritative quoter (Cetus pool::calculate_swap_result, DeepBook
pool::get_*_quantity_out). Net = proceeds - gas - DEEP fee. NEVER submits a tx.

This is the AMM<->DeepBook / DeepBook<->AMM arbitrage the routing graph gains by
adding DeepBook. Runs a short window; logs to deepbook_opps.jsonl.
"""

import json
import sys
import time

import cetus_rpc as c
import deepbook_rpc as db

GAS_SUI = 0.003 + 0.0012 * 2  # 2-hop round trip
INPUT_USD = [10, 50, 100, 500, 1000, 5000]

# Overlapping pairs: (mid token, USDC-denominated). base = USDC (USD directly).
# Cetus pool orientation noted as (type_a, type_b).
PAIRS = [
    {"mid": "SUI", "cetus_pool": "0xb8d7d9e66a60c239e7a60110efcf8de6c705580ed924d0dde141f4a0e2c90105",
     "cetus_a": "USDC", "cetus_b": "SUI", "db": "SUI_USDC"},   # DeepBook base=SUI quote=USDC
    {"mid": "DEEP", "cetus_pool": "0xa2f4e24dc234cf024bae1bd5b1275ab5bdc7c28dd1ec84dd98c2d012bbd315f0",
     "cetus_a": "DEEP", "cetus_b": "USDC", "db": "DEEP_USDC"},  # DeepBook base=DEEP quote=USDC
]
USDC = "0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC"
DEEP = "0xdeeb7a4662eec9f2f3def03fb937a663dddaa2e215b8078a284d026b7946c270::deep::DEEP"
SUI = "0x2::sui::SUI"
TYPE = {"USDC": USDC, "DEEP": DEEP, "SUI": SUI}


def cetus_isv(pool):
    d = c.rpc("sui_getObject", [pool, {"showOwner": True}])
    return int(d["data"]["owner"]["Shared"]["initial_shared_version"])


def deep_usd():
    p = db.POOLS["DEEP_USDC"]
    isv = db.get_isv(p["id"])
    q = db.quote_base_to_quote(p["id"], isv, p["base"], p["quote"], 1_000_000_000)  # 1000 DEEP (6dec)
    return q["quote_out"] / 1e6 / 1000.0  # USDC per 1 DEEP (large amt avoids sub-lot rounding to 0)


def amm_usdc_to_mid(pr, isv, usdc_native):
    # Cetus: USDC->mid. a2b = (from == type_a). from = USDC.
    a2b = (pr["cetus_a"] == "USDC")
    ta, tb = TYPE[pr["cetus_a"]], TYPE[pr["cetus_b"]]
    return c.cetus_quote(pr["cetus_pool"], isv, ta, tb, a2b, int(usdc_native))["amount_out"]


def amm_mid_to_usdc(pr, isv, mid_native):
    a2b = (pr["cetus_a"] == pr["mid"])
    ta, tb = TYPE[pr["cetus_a"]], TYPE[pr["cetus_b"]]
    return c.cetus_quote(pr["cetus_pool"], isv, ta, tb, a2b, int(mid_native))["amount_out"]


def db_usdc_to_mid(dbp, isv, usdc_native):
    # DeepBook base=mid, quote=USDC. USDC->mid = spend quote, get base.
    q = db.quote_quote_to_base(dbp["id"], isv, dbp["base"], dbp["quote"], int(usdc_native))
    return q["base_out"], q["deep_required"]


def db_mid_to_usdc(dbp, isv, mid_native):
    q = db.quote_base_to_quote(dbp["id"], isv, dbp["base"], dbp["quote"], int(mid_native))
    return q["quote_out"], q["deep_required"]


def scan(rounds, sleep_s=15):
    sui_usd_p = db.POOLS["SUI_USDC"]
    logf = open("deepbook_opps.jsonl", "a")
    for rnd in range(1, rounds + 1):
        try:
            d_usd = deep_usd()
            db_isv = {k: db.get_isv(v["id"]) for k, v in db.POOLS.items()}
            sui_isv = db_isv["SUI_USDC"]
            sui_usd = db.quote_base_to_quote(sui_usd_p["id"], sui_isv, sui_usd_p["base"], sui_usd_p["quote"], 1_000_000_000)["quote_out"] / 1e6
            gas_usd = GAS_SUI * sui_usd
            deep_unit_usd = d_usd  # USD per 1 DEEP
        except Exception as e:
            print("setup error", e); time.sleep(sleep_s); continue
        found = 0
        for pr in PAIRS:
            try:
                cisv = cetus_isv(pr["cetus_pool"])
                dbp = db.POOLS[pr["db"]]
                disv = db_isv[pr["db"]]
            except Exception as e:
                print("pool error", e); continue
            for usd in INPUT_USD:
                usdc0 = usd * 1e6  # USDC 6dec
                for direction in ("cetus->deepbook", "deepbook->cetus"):
                    try:
                        deep_fee_native = 0
                        if direction == "cetus->deepbook":
                            mid = amm_usdc_to_mid(pr, cisv, usdc0)
                            usdc1, df = db_mid_to_usdc(dbp, disv, mid)
                            deep_fee_native += df
                        else:
                            mid, df = db_usdc_to_mid(dbp, disv, usdc0)
                            deep_fee_native += df
                            usdc1 = amm_mid_to_usdc(pr, cisv, mid)
                    except Exception:
                        continue
                    gross_usd = (usdc1 - usdc0) / 1e6
                    deep_fee_usd = deep_fee_native / 1e6 * deep_unit_usd
                    net = gross_usd - gas_usd - deep_fee_usd
                    rec = {"ts": time.time(), "round": rnd, "pair": f"USDC/{pr['mid']}",
                           "direction": direction, "size_usd": usd, "gross_usd": round(gross_usd, 6),
                           "gas_usd": round(gas_usd, 6), "deep_fee_usd": round(deep_fee_usd, 6),
                           "net_usd": round(net, 6), "venues": ["cetus", "deepbook"]}
                    logf.write(json.dumps(rec) + "\n")
                    if net > 0:
                        found += 1
            logf.flush()
        print(f"round {rnd} SUI=${sui_usd:.4f} DEEP=${deep_unit_usd:.5f} gas=${gas_usd:.5f} profitable={found}")
        if rnd < rounds:
            time.sleep(sleep_s)
    logf.close()


if __name__ == "__main__":
    scan(int(sys.argv[1]) if len(sys.argv) > 1 else 6)
