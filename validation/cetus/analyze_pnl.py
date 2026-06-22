"""Analyze paper_trades.jsonl: profit by route / token-set / DEX combo, top &
bottom opportunities, cumulative PnL by token. Profit metric = simulated_net_usd
(the authoritative dry-run net after gas). "Paper profit" = records the bot would
execute (would_execute / sim_net > 0)."""

import json
import sys
from collections import Counter, defaultdict

PATH = sys.argv[1] if len(sys.argv) > 1 else "paper_trades.jsonl"
rows = [json.loads(l) for l in open(PATH) if l.strip()]
prof = [r for r in rows if r.get("would_execute")]  # sim_net > 0


def net(r):
    return r["simulated_net_usd"]


def route_key(r):
    return " -> ".join(r["path"])


def tokenset_key(r):
    return ",".join(sorted(set(r["path"])))


def dex_key(r):
    c = Counter(r["venues"])
    return "+".join(f"{k}x{v}" for k, v in sorted(c.items()))


def episodes(records):
    """Collapse consecutive same-route detections into episodes (a persistent
    dislocation reappears every round; one capture per episode is realistic)."""
    by_route = defaultdict(list)
    for r in records:
        by_route[route_key(r)].append(r)
    eps = 0
    best_sum = 0.0
    for route, rs in by_route.items():
        rs.sort(key=lambda r: r["ts"])
        gap = 0
        prev = None
        for r in rs:
            if prev is None or r["ts"] - prev > 120:  # >2 min gap = new episode
                eps += 1
                best_sum += net(r)
            prev = r["ts"]
    return eps, best_sum


def agg(records, keyfn, topn=20):
    d = defaultdict(lambda: [0, 0.0])
    for r in records:
        k = keyfn(r)
        d[k][0] += 1
        d[k][1] += net(r)
    return sorted(d.items(), key=lambda kv: -kv[1][1])[:topn]


def hdr(t):
    print("\n" + "=" * 78 + f"\n{t}\n" + "=" * 78)


span_h = (max(r["ts"] for r in rows) - min(r["ts"] for r in rows)) / 3600
total_prof = sum(net(r) for r in prof)
ep_count, ep_sum = episodes(prof)

hdr("OVERVIEW")
print(f"records (detections): {len(rows)}   window: {span_h:.2f} h")
print(f"profitable (would_execute, sim_net>0): {len(prof)}  ({len(prof)/span_h:.1f}/h)")
print(f"raw paper P&L (sum over every profitable detection): ${total_prof:.4f}")
print(f"distinct profitable episodes (dedup persistent, >2min gap): {ep_count}"
      f"  -> realistic paper P&L (1 capture/episode): ${ep_sum:.4f}")
print(f"loss if you executed EVERY detection (incl. negatives): ${sum(net(r) for r in rows):.4f}")

hdr("1. PROFIT BY ROUTE (profitable detections, top 20 by total $)")
print(f"{'route':<48}{'n':>5}{'total$':>11}{'mean$':>11}")
for k, (n, s) in agg(prof, route_key):
    print(f"{k:<48}{n:>5}{s:>11.4f}{s/n:>11.5f}")

hdr("2. PROFIT BY TOKEN SET (distinct tokens in the cycle)")
print(f"{'token set':<48}{'n':>5}{'total$':>11}{'mean$':>11}")
for k, (n, s) in agg(prof, tokenset_key):
    print(f"{k:<48}{n:>5}{s:>11.4f}{s/n:>11.5f}")

hdr("3. PROFIT BY DEX COMBINATION")
print(f"{'venues':<40}{'n':>6}{'total$':>12}{'mean$':>11}")
for k, (n, s) in agg(prof, dex_key):
    print(f"{k:<40}{n:>6}{s:>12.4f}{s/n:>11.5f}")

hdr("4. TOP 20 MOST PROFITABLE OPPORTUNITIES (by sim_net $)")
print(f"{'sim_net$':>10}{'edge_bp':>9}{'sz$':>6}  route / venues")
for r in sorted(rows, key=net, reverse=True)[:20]:
    print(f"{net(r):>10.4f}{r['edge_bps']:>9.1f}{r['size_usd']:>6}  {route_key(r)}  {r['venues']}")

hdr("5. TOP 20 LEAST PROFITABLE OPPORTUNITIES (by sim_net $)")
print(f"{'sim_net$':>10}{'edge_bp':>9}{'sz$':>6}  route / venues")
for r in sorted(rows, key=net)[:20]:
    print(f"{net(r):>10.4f}{r['edge_bps']:>9.1f}{r['size_usd']:>6}  {route_key(r)}  {r['venues']}")

hdr("6. CUMULATIVE PnL BY TOKEN")
# (a) realized paper PnL by base token
by_base = defaultdict(lambda: [0, 0.0])
for r in prof:
    by_base[r["base"]][0] += 1
    by_base[r["base"]][1] += net(r)
print("(a) realized paper PnL by BASE token:")
for k, (n, s) in sorted(by_base.items(), key=lambda kv: -kv[1][1]):
    print(f"    {k:<10} n={n:<5} ${s:.4f}")
# (b) involvement: realized PnL summed over every token in the route
by_tok = defaultdict(lambda: [0, 0.0])
for r in prof:
    for t in set(r["path"]):
        by_tok[t][0] += 1
        by_tok[t][1] += net(r)
print("(b) realized PnL by TOKEN INVOLVED (a route's PnL credited to each token it touches):")
for k, (n, s) in sorted(by_tok.items(), key=lambda kv: -kv[1][1]):
    print(f"    {k:<10} routes={n:<5} ${s:.4f}")

hdr("CONCENTRATION — which routes generate most of the paper profit")
ranked = agg(prof, route_key, topn=10**9)
cum = 0.0
for i, (k, (n, s)) in enumerate(ranked, 1):
    cum += s
    print(f"  {i:>2}. {k:<46} ${s:>9.4f}  cum {cum/total_prof*100:>5.1f}%")
    if cum / total_prof >= 0.95:
        print(f"  -> top {i} routes = {cum/total_prof*100:.1f}% of total paper profit")
        break
