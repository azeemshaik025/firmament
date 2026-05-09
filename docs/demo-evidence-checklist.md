# Demo Evidence Checklist

Internal checklist. This file is ignored by Mintlify.

Use one timestamped folder per run:

```bash
export DEMO_RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "demo-evidence/$DEMO_RUN_ID"/{logs,api,screenshots,sqlite,tx}
```

## Before the run

Record repo state and policy:

```bash
git status --short > "demo-evidence/$DEMO_RUN_ID/git-status.txt"
git rev-parse HEAD > "demo-evidence/$DEMO_RUN_ID/git-head.txt"
cp config.toml "demo-evidence/$DEMO_RUN_ID/config.toml"
rg -n 'max_action_notional_usd|max_cumulative_automation_notional_usd|non_stable_asset_exception_notional_usd|max_refill_notional_usd|max_trade_notional_usd|max_daily_notional_usd|max_non_stable_asset_notional_usd' \
  config.toml > "demo-evidence/$DEMO_RUN_ID/caps.txt"
```

Confirm before live mode:

- `runtime.enable_protocol_workers = true`
- cbBTC mint is not `VERIFY_CBBTC_SOLANA_MINT_BEFORE_LIVE_USE`
- maker wallet is funded only for the planned tiny run
- caps match the demo policy

## Production surface polish

Run the static and production-build checks before recording or submitting:

```bash
npm run web:polish
npm run app:build
npm run landing:build
npm run docs:check
```

Manually hard refresh the deployed landing page, `/app`, `/app/runtime`, and
`/docs` with cache disabled. Confirm the Firmament wordmark does not visibly
swap from a fallback font, the `/app` and `/docs` links resolve from the landing
domain, the browser tab shows the custom Firmament favicon instead of the
deployment default, and mobile screenshots have no clipped buttons, counters, or
headings.

## Logs

```bash
cargo run --release 2>&1 | tee "demo-evidence/$DEMO_RUN_ID/logs/runtime.raw.log"
scripts/redact-demo-log.sh \
  "demo-evidence/$DEMO_RUN_ID/logs/runtime.raw.log" \
  > "demo-evidence/$DEMO_RUN_ID/logs/runtime.redacted.log"
```

## API snapshots

Capture before and after the operator flow:

```bash
curl -s http://127.0.0.1:5050/health \
  | jq . > "demo-evidence/$DEMO_RUN_ID/api/health.json"

curl -s http://127.0.0.1:5050/v1/assets \
  | jq . > "demo-evidence/$DEMO_RUN_ID/api/assets.json"

curl -s http://127.0.0.1:5050/v1/runtime/state \
  | jq . > "demo-evidence/$DEMO_RUN_ID/api/state-before.json"

curl -s http://127.0.0.1:5050/v1/runtime/events \
  | jq . > "demo-evidence/$DEMO_RUN_ID/api/events-before.json"

curl -s http://127.0.0.1:5050/v1/runtime/ledger \
  | jq . > "demo-evidence/$DEMO_RUN_ID/api/ledger-before.json"

curl -s 'http://127.0.0.1:5050/v1/runtime/trades?limit=10' \
  | jq . > "demo-evidence/$DEMO_RUN_ID/api/trades-before.json"
```

After RFQ, settlement, and any operator repair checks:

```bash
curl -s http://127.0.0.1:5050/v1/runtime/state \
  | jq . > "demo-evidence/$DEMO_RUN_ID/api/state-after.json"

curl -s 'http://127.0.0.1:5050/v1/runtime/events?limit=100' \
  | jq . > "demo-evidence/$DEMO_RUN_ID/api/events-after.json"

curl -s http://127.0.0.1:5050/v1/runtime/ledger \
  | jq . > "demo-evidence/$DEMO_RUN_ID/api/ledger-after.json"

curl -s 'http://127.0.0.1:5050/v1/runtime/trades?limit=10' \
  | jq . > "demo-evidence/$DEMO_RUN_ID/api/trades-after.json"
```

For each accepted trade:

```bash
curl -s "http://127.0.0.1:5050/v1/trades/<trade-id>" \
  | jq . > "demo-evidence/$DEMO_RUN_ID/api/trade-<trade-id>.json"
```

For an in-progress wallet settlement, capture recovery and expiry behavior:

```bash
curl -s -X POST "http://127.0.0.1:5050/v1/trades/<trade-id>/resume" \
  | jq . > "demo-evidence/$DEMO_RUN_ID/api/trade-<trade-id>-resume.json"

curl -s -X POST "http://127.0.0.1:5050/v1/trades/<trade-id>/taker-refund" \
  -H 'content-type: application/json' \
  -d '{}' \
  | jq . > "demo-evidence/$DEMO_RUN_ID/api/trade-<trade-id>-refund-prepare.json"
```

Only call the refund route after the taker lock is on chain and the HTLC expiry
has passed. Use the abandon route only before any lock signature:

```bash
curl -s -X POST "http://127.0.0.1:5050/v1/trades/<trade-id>/abandon" \
  -H 'content-type: application/json' \
  -d '{"secret_hash":"<browser-generated-secret-hash>"}' \
  | jq . > "demo-evidence/$DEMO_RUN_ID/api/trade-<trade-id>-abandon.json"
```

## Screenshots

Save screenshots under `demo-evidence/$DEMO_RUN_ID/screenshots/`.

Capture:

- `/app` before quote
- `/app` after accepted or rejected RFQ
- `/app` during wallet settlement
- `/app` after a browser refresh restores an in-progress settlement
- `/app` wallet-specific trade history drawer for the connected wallet
- `/app` refund-after-expiry state if the refund path is part of the run
- `/app/runtime` after startup
- `/app/runtime` after settlement or operator checks

## Transaction links

Extract signatures from captured API files:

```bash
jq -r '.. | objects | .signature? // empty' \
  "demo-evidence/$DEMO_RUN_ID/api/"*.json \
  | sort -u \
  | tee "demo-evidence/$DEMO_RUN_ID/tx/signatures.txt" \
  | awk '{print "https://solscan.io/tx/" $0}' \
  > "demo-evidence/$DEMO_RUN_ID/tx/solscan-links.txt"
```

Expected when executed:

- taker lock signature
- maker lock signature
- taker redeem signature
- maker redeem signature
- Jupiter rebalance signature, if a rebalance ran
- Circle Gateway deposit/refill signature or provider transfer ID, if Gateway ran

## SQLite checks

If the run uses local SQLite:

```bash
cp <runtime-sqlite-path> "demo-evidence/$DEMO_RUN_ID/sqlite/runtime.sqlite"

sqlite3 "demo-evidence/$DEMO_RUN_ID/sqlite/runtime.sqlite" \
  'SELECT COUNT(*) FROM ledger_transactions;' \
  > "demo-evidence/$DEMO_RUN_ID/sqlite/ledger-transaction-count.txt"

sqlite3 "demo-evidence/$DEMO_RUN_ID/sqlite/runtime.sqlite" \
  'SELECT COUNT(*) FROM runtime_trades;' \
  > "demo-evidence/$DEMO_RUN_ID/sqlite/runtime-trade-count.txt"

sqlite3 "demo-evidence/$DEMO_RUN_ID/sqlite/runtime.sqlite" \
  'SELECT COUNT(*) FROM runtime_wallet_settlements;' \
  > "demo-evidence/$DEMO_RUN_ID/sqlite/runtime-wallet-settlement-count.txt"

sqlite3 "demo-evidence/$DEMO_RUN_ID/sqlite/runtime.sqlite" \
  'SELECT account_type, asset_id, qualifier, SUM(CAST(amount_raw AS INTEGER)) AS balance_raw
   FROM ledger_entries
   GROUP BY account_type, asset_id, qualifier
   ORDER BY account_type, asset_id, qualifier;' \
  > "demo-evidence/$DEMO_RUN_ID/sqlite/ledger-balances.tsv"
```

Check:

- ledger transactions have entries
- accepted trades create durable trade or active settlement records
- entries net to zero per transaction and asset
- public ledger endpoint reports `healthy: true`

## Redaction

Before sharing, search for secrets:

```bash
rg -n 'PRIVATE_KEY|KEYPAIR|SECRET|TOKEN|API_KEY|RPC_URL|WALLET_ID|preimage' "demo-evidence/$DEMO_RUN_ID"
```

Manually inspect every match. Redact private keys, keypair JSON, API keys,
credentialed RPC URLs, unapproved Gateway identifiers, and any browser-local
preimage accidentally captured in screenshots or logs.
