#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  printf 'Usage: scripts/redact-demo-log.sh <log-file>\n' >&2
  exit 2
fi

sed -E \
  -e 's#(SOLANA_RPC_URL=)[^[:space:]]+#\1<redacted>#g' \
  -e 's#(MAKER_PRIVATE_KEY=)[^[:space:]]+#\1<redacted>#g' \
  -e 's#(TAKER_PRIVATE_KEY=)[^[:space:]]+#\1<redacted>#g' \
  -e 's#(MAKER_KEYPAIR_JSON=)[^[:space:]]+#\1<redacted>#g' \
  -e 's#(TAKER_KEYPAIR_JSON=)[^[:space:]]+#\1<redacted>#g' \
  -e 's#(MAKER_KEYPAIR_PATH=)[^[:space:]]+#\1<redacted>#g' \
  -e 's#(TAKER_KEYPAIR_PATH=)[^[:space:]]+#\1<redacted>#g' \
  -e 's#(JUPITER_API_KEY=)[^[:space:]]+#\1<redacted>#g' \
  "$1"
