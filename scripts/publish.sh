#!/usr/bin/env bash
# Publish the arbitrage_system Move package and print the new package id.
#
# Uses your CURRENTLY ACTIVE sui env + address (check first!):
#   sui client active-env       # mainnet or testnet — must match your intent
#   sui client active-address   # the funded deployer
#   sui client gas              # confirm it has SUI for gas
#
# Costs real gas. Does NOT trade. See docs/testnet-runbook.md.
set -euo pipefail
cd "$(dirname "$0")/.."

BUDGET="${1:-200000000}"   # 0.2 SUI default

echo "active-env:     $(sui client active-env 2>/dev/null || echo '?')"
echo "active-address: $(sui client active-address 2>/dev/null || echo '?')"
echo "publishing arbitrage_system (gas budget ${BUDGET})..."
read -r -p "Proceed? [y/N] " ok; [ "$ok" = "y" ] || { echo "aborted"; exit 1; }

OUT="$(sui client publish --gas-budget "$BUDGET" --json)"

# Extract the published package id (the object with type 'published').
PKG="$(printf '%s' "$OUT" | python3 -c '
import json,sys
d=json.load(sys.stdin)
for c in d.get("objectChanges", []):
    if c.get("type")=="published":
        print(c["packageId"]); break
')"

echo
echo "==================================================================="
echo "ARB_PACKAGE_ID=${PKG:-<not found — inspect publish output>}"
echo "==================================================================="
echo "Put this in .env (ARB_PACKAGE_ID). Next: docs/testnet-runbook.md step 2."
