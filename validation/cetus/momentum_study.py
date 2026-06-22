"""Momentum profitability study over paper_trades.jsonl (combined Cetus/Turbos/
Kriya/Momentum window). Profit metric = simulated_net_usd (dry-run net after gas).
'Paper profit' = would_execute (sim_net>0). Read-only analysis."""

import json
import statistics
import sys
from collections import Counter, defaultdict

PATH = sys.argv[1] if len(sys.argv) > 1 else "paper_trades.jsonl"
rows = [json.loads(l) for l in open(PATH) if l.strip()]
prof = [r for r in rows if r.get("would_execute")]
span_h = (max(r["ts"] for r in rows) - min(r["ts"] for r in rows)) / 3600 if rows else 0


def net(r):
    return r["simulated_net_usd"]


def hdr(t):
    print("\n" + "=" * 74 + f"\n{t}\n" + "=" * 74)


hdr("OVERVIEW")
print(f"records {len(rows)} | window {span_h:.2f}h | profitable {len(prof)} "
      f"({len(prof)/span_h:.1f}/h)" if span_h else f"records {len(rows)}")
raw = sum(net(r) for r in prof)
print(f"raw paper P&L (profitable detections): ${raw:.4f}")
mm_prof = [r for r in prof if "momentum" in r["venues"]]
print(f"profitable opps touching Momentum: {len(mm_prof)}  (${sum(net(r) for r in mm_prof):.4f})")

hdr("1. PROFIT BY VENUE (involvement: a cycle's net credited to each venue it uses)")
byv = defaultdict(lambda: [0, 0.0])
for r in prof:
    for v in set(r["venues"]):
        byv[v][0] += 1
        byv[v][1] += net(r)
for v, (n, s) in sorted(byv.items(), key=lambda kv: -kv[1][1]):
    print(f"  {v:<10} routes={n:<5} ${s:.4f}")

hdr("2. PROFIT BY DEX COMBINATION")
byc = defaultdict(lambda: [0, 0.0])
for r in prof:
    k = "+".join(f"{a}x{b}" for a, b in sorted(Counter(r["venues"]).items()))
    byc[k][0] += 1
    byc[k][1] += net(r)
for k, (n, s) in sorted(byc.items(), key=lambda kv: -kv[1][1])[:15]:
    print(f"  {k:<34} n={n:<4} ${s:.4f}")

hdr("3. PROFIT BY ROUTE (Momentum-touching, top 15)")
byr = defaultdict(lambda: [0, 0.0])
for r in mm_prof:
    k = " -> ".join(r["path"])
    byr[k][0] += 1
    byr[k][1] += net(r)
for k, (n, s) in sorted(byr.items(), key=lambda kv: -kv[1][1])[:15]:
    print(f"  {k:<46} n={n:<4} ${s:.4f}")

hdr("4. PROFIT BY TOKEN (involvement, profitable)")
byt = defaultdict(lambda: [0, 0.0])
for r in prof:
    for t in set(r["path"]):
        byt[t][0] += 1
        byt[t][1] += net(r)
for t, (n, s) in sorted(byt.items(), key=lambda kv: -kv[1][1]):
    print(f"  {t:<10} routes={n:<5} ${s:.4f}")

hdr("5/6. FREQUENCY + DAILY/WEEKLY EXTRAPOLATION")
if span_h:
    print(f"profitable opp frequency: {len(prof)/span_h:.2f}/h")
    print(f"paper P&L: ${raw:.4f} over {span_h:.2f}h -> ${raw/span_h*24:.2f}/day -> ${raw/span_h*24*7:.2f}/week")
    print(f"Momentum-touching P&L: ${sum(net(r) for r in mm_prof):.4f} "
          f"-> ${sum(net(r) for r in mm_prof)/span_h*24:.2f}/day")

hdr("7. MOMENTUM-EXCLUSIVE OPPORTUNITIES (profitable cycles that use a Momentum leg)")
print(f"count: {len(mm_prof)}  (these did not exist before Momentum was added)")
for r in sorted(mm_prof, key=net, reverse=True)[:15]:
    print(f"  net=${net(r):>8.4f} edge={r['edge_bps']:>7.1f}bp {' -> '.join(r['path'])} {r['venues']} auth={r['authoritative']}")
