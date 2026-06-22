"""Read-only Aftermath integration: aggregated route quotes via the public REST
router API. Used as a PRICE / LIQUIDITY / ROUTE-QUALITY ORACLE and benchmark — not
a graph edge we execute. NEVER submits a transaction.

POST https://aftermath.finance/api/router/trade-route
  body: {coinInType, coinOutType, coinInAmount}
  resp: {routes[], coinIn, coinOut{amount "..n"}, spotPrice, netTradeFeePercentage}
"""

import json
import subprocess

API = "https://aftermath.finance/api/router/trade-route"
UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36"


def quote(coin_in, coin_out, amount_in):
    # Aftermath sits behind Cloudflare; use curl with a browser UA (urllib UA is blocked).
    body = json.dumps({"coinInType": coin_in, "coinOutType": coin_out, "coinInAmount": str(int(amount_in))})
    out = subprocess.run(
        ["curl", "-s", "-m", "30", "-X", "POST", API, "-H", "Content-Type: application/json",
         "-H", f"User-Agent: {UA}", "-d", body],
        capture_output=True, text=True,
    ).stdout
    d = json.loads(out)
    out = int(str(d["coinOut"]["amount"]).rstrip("n"))
    prots = sorted({p.get("protocolName") for r in d.get("routes", []) for p in r.get("paths", [])})
    return {"amount_out": out, "protocols": prots, "spot_price": d.get("spotPrice"),
            "fee_pct": d.get("netTradeFeePercentage")}


if __name__ == "__main__":
    import cetus_rpc as c
    import deepbook_rpc as db

    SUI = "0x2::sui::SUI"
    USDC = "0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC"
    DEEP = "0xdeeb7a4662eec9f2f3def03fb937a663dddaa2e215b8078a284d026b7946c270::deep::DEEP"

    # our authoritative SUI->USDC via Cetus (pool USDC/SUI; SUI->USDC is b->a) and DeepBook
    cetus_sui_usdc = "0xb8d7d9e66a60c239e7a60110efcf8de6c705580ed924d0dde141f4a0e2c90105"
    cisv = c.rpc("sui_getObject", [cetus_sui_usdc, {"showOwner": True}])["data"]["owner"]["Shared"]["initial_shared_version"]
    dbp = db.POOLS["SUI_USDC"]
    disv = db.get_isv(dbp["id"])

    print(f"{'size SUI':>10} {'aftermath':>12} {'cetus':>12} {'deepbook':>12}  best_vs_AF  AF_route")
    for sui in (1_000_000_000, 50_000_000_000, 500_000_000_000):
        af = quote(SUI, USDC, sui)
        ce = c.cetus_quote(cetus_sui_usdc, int(cisv), USDC, SUI, False, sui)["amount_out"]  # SUI->USDC
        db_q = db.quote_base_to_quote(dbp["id"], disv, dbp["base"], dbp["quote"], sui)["quote_out"]
        best = max(ce, db_q)
        ratio = best / af["amount_out"] if af["amount_out"] else 0
        print(f"{sui/1e9:>10.0f} {af['amount_out']:>12} {ce:>12} {db_q:>12}  {ratio:>9.4f}  {af['protocols']}")
