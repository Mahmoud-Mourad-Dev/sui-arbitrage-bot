#!/usr/bin/env bash
# Launch the 24-hour paper-trading window (read-only; never submits a tx).
# Detached so it survives terminal close; logs to paper_trade_24h.log.
#
# Usage:   ./run_24h.sh            # 24h (1440 min)
#          ./run_24h.sh 720        # custom minutes
# Report:  python3 paper_trade.py report   # any time, on the growing log
#
# The framework checkpoints every round to paper_trades.jsonl, so `report` works
# while it runs and the run is resumable (append-only log).

set -euo pipefail
cd "$(dirname "$0")"
MINUTES="${1:-1440}"

echo "Refreshing pool universe..."
python3 mv_scan.py discover

echo "Starting ${MINUTES}-minute paper-trading window (detached)..."
# Subshell + nohup reparents to launchd/init so it survives terminal/session exit
# (macOS has no setsid/tmux by default). stdin from /dev/null fully detaches.
( nohup python3 -u paper_trade.py run "$MINUTES" > paper_trade_24h.log 2>&1 < /dev/null & )
sleep 1
PID="$(pgrep -f "paper_trade.py run ${MINUTES}" | head -1)"
echo "PID ${PID:-?}  log: $(pwd)/paper_trade_24h.log"
echo "Monitor:  tail -f paper_trade_24h.log   |   Report: python3 paper_trade.py report"
echo "Stop:     kill ${PID:-<pid>}"
