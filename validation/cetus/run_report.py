"""Clean-window report for one paper-trading run (reads paper_trades.jsonl, which
holds ONLY the current run — the prior log was archived before launch). Read-only.

Usage: python3 run_report.py [paper_trades.jsonl] [start_epoch] [rounds]
Prints a console summary and writes run_report_<date>.md.
"""
import json, sys, math, time, datetime, collections

PATH = sys.argv[1] if len(sys.argv) > 1 else "paper_trades.jsonl"
START = float(sys.argv[2]) if len(sys.argv) > 2 else None
ROUNDS = int(sys.argv[3]) if len(sys.argv) > 3 else None

recs = [json.loads(l) for l in open(PATH) if l.strip()]
if not recs:
    print("no records"); sys.exit(0)

ts = [r["ts"] for r in recs]
t0, t1 = min(ts), max(ts)
start = START if START else t0
dur_h = (t1 - start) / 3600.0 or 1e-9

prof = [r for r in recs if r.get("simulated_net_usd", 0) > 0]
wexec = [r for r in recs if r.get("would_execute")]

def route_str(r): return " -> ".join(r["path"])
def dex_combo(r):
    c = collections.Counter(r["venues"])
    return " + ".join(f"{k}x{v}" for k, v in sorted(c.items()))
def pairs(path): return [f"{path[i]}/{path[i+1]}" for i in range(len(path)-1)]

nets = sorted(r["simulated_net_usd"] for r in prof)
def pct(p):
    if not nets: return 0.0
    return nets[min(len(nets)-1, int(p*len(nets)))]
gross = sum(nets)

# dedup: one capture per persistent dislocation (same route within 120s -> one episode, max net)
episodes = []
last = {}
for r in sorted(prof, key=lambda r: r["ts"]):
    k = route_str(r)
    if k in last and r["ts"] - last[k][0] <= 120:
        ep = episodes[last[k][1]]
        ep["net"] = max(ep["net"], r["simulated_net_usd"])
    else:
        episodes.append({"net": r["simulated_net_usd"], "route": k, "rec": r})
    last[k] = (r["ts"], len(episodes)-1)
dedup = sum(e["net"] for e in episodes)

# friction model (arb), mirrors offchain/src/frictions.rs defaults
LAT, COMP, HL, GAS = 800.0, 3.0, 250.0, 0.03
def p_alive(bps): return math.exp(-LAT/(HL*bps)) if bps > 0 else 0.0
pwin = 1.0/(1.0+COMP)
adj = sum(p_alive(e["rec"]["edge_bps"]) * (pwin*e["net"] - (1-pwin)*GAS) for e in episodes)
capture = (adj/dedup) if dedup > 0 else 0.0
daily_adj = adj * 24.0/dur_h

def by(keyfn, items):
    d = collections.defaultdict(lambda: [0.0, 0])
    for r in items:
        d[keyfn(r)][0] += r["simulated_net_usd"]; d[keyfn(r)][1] += 1
    return sorted(d.items(), key=lambda kv: -kv[1][0])

routes = by(route_str, prof)
dexes = by(dex_combo, prof)
# token involvement profit
tok = collections.defaultdict(float)
for r in prof:
    for t in set(r["path"]): tok[t] += r["simulated_net_usd"]
tok = sorted(tok.items(), key=lambda kv: -kv[1])
# token pairs
pair_prof = collections.defaultdict(float); pair_cnt = collections.Counter()
for r in prof:
    for p in pairs(r["path"]):
        canon = "/".join(sorted(p.split("/")))
        pair_prof[canon] += r["simulated_net_usd"]; pair_cnt[canon] += 1
pair_prof = sorted(pair_prof.items(), key=lambda kv: -kv[1])
# dex triplets/pairs by venue multiset length
trip = by(lambda r: dex_combo(r), [r for r in prof if len(r["venues"]) >= 3])
thresholds = [0.10, 0.25, 0.50, 1.00, 2.00, 5.00]
dist = {t: sum(1 for n in nets if n > t) for t in thresholds}

start_s = datetime.datetime.fromtimestamp(start).strftime("%Y-%m-%d %H:%M:%S")
stop_s = datetime.datetime.fromtimestamp(t1).strftime("%Y-%m-%d %H:%M:%S")

L = []
def P(s=""): L.append(s); print(s)

P(f"# 10–12h Authoritative Paper-Trading Report ({start_s} → {stop_s})\n")
P("## Run Summary")
P(f"- start: {start_s}  ·  stop: {stop_s}  ·  duration: {dur_h:.2f} h")
P(f"- rounds completed: {ROUNDS if ROUNDS else 'n/a'}  ·  pools: 86  ·  venues: 5 (cetus,turbos,kriya,momentum,bluefin)")
P(f"- records (detections): {len(recs)}  ·  authoritative dry-runs: {sum(1 for r in recs if r.get('authoritative'))}")
P("\n## Opportunity Statistics")
P(f"- detections: {len(recs)}  ({len(recs)/dur_h:.1f}/h)")
P(f"- profitable (sim_net>0): {len(prof)}  ({len(prof)/dur_h:.2f}/h)")
P(f"- would_execute: {len(wexec)}  (rate {100*len(wexec)/max(1,len(recs)):.2f}% of detections)")
P("\n## Profitability (USD, simulated authoritative net)")
if prof:
    P(f"- gross paper P&L: ${gross:.4f}  ·  per day: ${gross*24/dur_h:.2f}")
    P(f"- deduplicated P&L: ${dedup:.4f} ({len(episodes)} episodes)  ·  per day: ${dedup*24/dur_h:.2f}")
    P(f"- avg ${gross/len(prof):.4f} · median ${pct(0.5):.4f} · p95 ${pct(0.95):.4f} · max ${nets[-1]:.4f} · min ${nets[0]:.4f}")
else:
    P("- no profitable (sim_net>0) opportunities in this window")
P("\n## Route Analysis (top 20 by profit)")
for r, (pnl, n) in routes[:20]:
    P(f"- ${pnl:.4f} ({n}x)  {r}")
if routes:
    top = routes[0][1][0]
    P(f"- route concentration: top route = {100*top/gross:.1f}%  ·  top3 = {100*sum(v[0] for _,v in routes[:3])/gross:.1f}%" if gross>0 else "")
P("\n## DEX Analysis (profit by venue combination)")
for d, (pnl, n) in dexes[:12]:
    P(f"- ${pnl:.4f} ({n}x)  {d}")
P("\n## Token Analysis")
P("profit by token (involvement): " + ", ".join(f"{t} ${v:.2f}" for t, v in tok[:12]))
P("top profitable pairs: " + ", ".join(f"{p} ${v:.3f}" for p, v in pair_prof[:8]))
P("most frequent pairs: " + ", ".join(f"{p} ({pair_cnt[p]})" for p, _ in sorted(pair_cnt.items(), key=lambda kv:-kv[1])[:8]))
P("\n## Opportunity Distribution (profit >)")
for t in thresholds:
    P(f"- > ${t:.2f}: {dist[t]}")
P("\n## Execution Readiness (arb friction model: lat=800ms, comp=3, gas=$0.03)")
P(f"- estimated capture rate: {100*capture:.1f}%")
P(f"- net after frictions (window): ${adj:.4f}")
P(f"- expected daily net: ${daily_adj:.2f}")
P("\n## Top 20 opportunities")
for r in sorted(prof, key=lambda r:-r["simulated_net_usd"])[:20]:
    P(f"- ${r['simulated_net_usd']:.4f}  {r['edge_bps']:.1f}bps  {route_str(r)}  [{dex_combo(r)}]")

open(f"run_report_{datetime.date.today()}.md", "w").write("\n".join(L))
print(f"\n[written run_report_{datetime.date.today()}.md]")
