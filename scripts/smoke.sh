#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

usage() {
  cat <<'USAGE'
Usage: scripts/smoke.sh <group>

Groups:
  unit              Run the full pure/default cargo test suite.
  fake-e2e          Run fake-adapter RFQ accept, automation, ledger, and P&L tests.
  api               Run local operator API route tests.
  solana            Run env-gated balance/ATA validation tests.
  jupiter-quote     Run env-gated Jupiter quote/order fetch tests.
  jupiter-cbbtc-quote Run env-gated Jupiter cbBTC route quote test.
  jupiter-cbbtc-swap Run explicit opt-in tiny Jupiter USDC->cbBTC swap test.
  jupiter-swap      Run explicit opt-in tiny Jupiter swap test.
  htlc              Run HTLC builder tests and the env-gated live HTLC gate.
  gateway-balance   Run env-gated Circle Gateway balance read test.
  gateway-mutating  Run explicit opt-in Gateway deposit/refill gate test.
  gateway-deposit   Run explicit opt-in 1 USDC Gateway deposit smoke test.
  gateway-refill    Run explicit opt-in 1 USDC Gateway refill smoke test.
  live-cbbtc-rfq    Run explicit opt-in tiny live USDC->cbBTC RFQ/HTLC smoke test.
  live-e2e-gated    Run explicit opt-in live end-to-end gate tests.
  all-non-mutating  Run unit, fake-e2e, api, Solana, Jupiter quote/cbBTC quote, HTLC, and Gateway balance groups.

Live tests skip cleanly unless their required environment variables are present.
Mutating groups refuse to start unless their explicit RUN_LIVE_* opt-ins are set.
USAGE
}

run() {
  printf '\n==> %s\n' "$*"
  "$@"
}

require_env_value() {
  local name="$1"
  local expected="$2"
  local actual="${!name:-}"
  if [[ "$actual" != "$expected" ]]; then
    printf 'Refusing mutating smoke: %s must be %s\n' "$name" "$expected" >&2
    exit 2
  fi
}

require_any_env() {
  local label="$1"
  shift
  local name
  for name in "$@"; do
    if [[ -n "${!name:-}" ]]; then
      return
    fi
  done
  printf 'Refusing mutating smoke: set one of %s for %s\n' "$*" "$label" >&2
  exit 2
}

require_nonempty_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    printf 'Refusing mutating smoke: %s must be set\n' "$name" >&2
    exit 2
  fi
}

require_tiny_jupiter_swap_cap() {
  local amount="${LIVE_JUPITER_SWAP_AMOUNT_RAW:-1000000}"
  if ! [[ "$amount" =~ ^[0-9]+$ ]]; then
    printf 'Refusing mutating smoke: LIVE_JUPITER_SWAP_AMOUNT_RAW must be an integer\n' >&2
    exit 2
  fi
  if (( amount > 10000000 )); then
    printf 'Refusing mutating smoke: LIVE_JUPITER_SWAP_AMOUNT_RAW=%s exceeds 10000000 lamports\n' "$amount" >&2
    exit 2
  fi
}

require_tiny_gateway_deposit_cap() {
  local amount="${LIVE_GATEWAY_DEPOSIT_USDC_RAW:-1000000}"
  if ! [[ "$amount" =~ ^[0-9]+$ ]]; then
    printf 'Refusing mutating smoke: LIVE_GATEWAY_DEPOSIT_USDC_RAW must be an integer\n' >&2
    exit 2
  fi
  if (( amount < 1 || amount > 1000000 )); then
    printf 'Refusing mutating smoke: LIVE_GATEWAY_DEPOSIT_USDC_RAW=%s must be between 1 and 1000000 raw USDC\n' "$amount" >&2
    exit 2
  fi
}

require_tiny_gateway_refill_cap() {
  local amount="${LIVE_GATEWAY_REFILL_USDC_RAW:-1000000}"
  if ! [[ "$amount" =~ ^[0-9]+$ ]]; then
    printf 'Refusing mutating smoke: LIVE_GATEWAY_REFILL_USDC_RAW must be an integer\n' >&2
    exit 2
  fi
  if (( amount < 1 || amount > 1000000 )); then
    printf 'Refusing mutating smoke: LIVE_GATEWAY_REFILL_USDC_RAW=%s must be between 1 and 1000000 raw USDC\n' "$amount" >&2
    exit 2
  fi
}

require_tiny_gateway_refill_fee_cap() {
  local amount="${LIVE_GATEWAY_REFILL_MAX_FEE_RAW:-250000}"
  if ! [[ "$amount" =~ ^[0-9]+$ ]]; then
    printf 'Refusing mutating smoke: LIVE_GATEWAY_REFILL_MAX_FEE_RAW must be an integer\n' >&2
    exit 2
  fi
  if (( amount < 150000 || amount > 500000 )); then
    printf 'Refusing mutating smoke: LIVE_GATEWAY_REFILL_MAX_FEE_RAW=%s must be between 150000 and 500000 raw USDC\n' "$amount" >&2
    exit 2
  fi
}

require_tiny_cbbtc_quote_cap() {
  local amount="${LIVE_CBBTC_QUOTE_USDC_RAW:-5000000}"
  if ! [[ "$amount" =~ ^[0-9]+$ ]]; then
    printf 'Refusing cbBTC quote smoke: LIVE_CBBTC_QUOTE_USDC_RAW must be an integer\n' >&2
    exit 2
  fi
  if (( amount < 1 || amount > 5000000 )); then
    printf 'Refusing cbBTC quote smoke: LIVE_CBBTC_QUOTE_USDC_RAW=%s must be between 1 and 5000000 raw USDC\n' "$amount" >&2
    exit 2
  fi
}

require_tiny_cbbtc_swap_cap() {
  local amount="${LIVE_CBBTC_SWAP_USDC_RAW:-4000000}"
  if ! [[ "$amount" =~ ^[0-9]+$ ]]; then
    printf 'Refusing mutating smoke: LIVE_CBBTC_SWAP_USDC_RAW must be an integer\n' >&2
    exit 2
  fi
  if (( amount < 1 || amount > 5000000 )); then
    printf 'Refusing mutating smoke: LIVE_CBBTC_SWAP_USDC_RAW=%s must be between 1 and 5000000 raw USDC\n' "$amount" >&2
    exit 2
  fi
}

require_tiny_cbbtc_rfq_cap() {
  local amount="${LIVE_CBBTC_RFQ_USDC_RAW:-100000}"
  if ! [[ "$amount" =~ ^[0-9]+$ ]]; then
    printf 'Refusing mutating smoke: LIVE_CBBTC_RFQ_USDC_RAW must be an integer\n' >&2
    exit 2
  fi
  if (( amount < 1 || amount > 500000 )); then
    printf 'Refusing mutating smoke: LIVE_CBBTC_RFQ_USDC_RAW=%s must be between 1 and 500000 raw USDC\n' "$amount" >&2
    exit 2
  fi
}

group="${1:-all-non-mutating}"

case "$group" in
  unit)
    run cargo test
    ;;
  fake-e2e)
    run cargo test --test runtime_integration -- --nocapture
    ;;
  api)
    run cargo test --test api -- --nocapture
    ;;
  solana)
    run cargo test --test assets_wallets_solana wallets_live_loading_skips_without_env -- --nocapture
    run cargo test --test assets_wallets_solana solana_client_live_balance_and_ata_reads_skip_without_env -- --nocapture
    ;;
  jupiter-quote)
    run cargo test --lib jupiter_live_quote_fetch_skips_without_env -- --nocapture
    run cargo test --lib jupiter_live_order_fetch_skips_without_env -- --nocapture
    ;;
  jupiter-cbbtc-quote)
    require_tiny_cbbtc_quote_cap
    run cargo test --lib jupiter_live_cbbtc_route_fetch_skips_without_env -- --nocapture
    ;;
  jupiter-cbbtc-swap)
    require_env_value RUN_LIVE_SOLANA_TESTS 1
    require_env_value RUN_LIVE_JUPITER_CBBTC_SWAP_TESTS 1
    require_nonempty_env SOLANA_RPC_URL
    require_nonempty_env JUPITER_API_KEY
    require_any_env "maker wallet" MAKER_PRIVATE_KEY MAKER_KEYPAIR_JSON MAKER_KEYPAIR_PATH
    require_tiny_cbbtc_swap_cap
    run cargo test --lib jupiter_live_cbbtc_swap_skips_without_explicit_mutating_opt_in -- --nocapture
    ;;
  jupiter-swap)
    require_env_value RUN_LIVE_SOLANA_TESTS 1
    require_env_value RUN_LIVE_JUPITER_SWAP_TESTS 1
    require_any_env "maker wallet" MAKER_PRIVATE_KEY MAKER_KEYPAIR_JSON MAKER_KEYPAIR_PATH
    require_tiny_jupiter_swap_cap
    run cargo test --lib jupiter_live_swap_skips_without_explicit_mutating_opt_in -- --nocapture
    ;;
  htlc)
    run cargo test --lib adapters::solana::htlc::client::tests:: -- --nocapture
    ;;
  gateway-balance)
    run cargo test --lib live_gateway_balance_read_skips_without_env -- --nocapture
    ;;
  gateway-mutating)
    require_env_value RUN_LIVE_SOLANA_TESTS 1
    require_env_value RUN_LIVE_GATEWAY_TESTS 1
    require_env_value RUN_LIVE_GATEWAY_MUTATING_TESTS 1
    require_any_env "maker wallet" MAKER_PRIVATE_KEY MAKER_KEYPAIR_JSON MAKER_KEYPAIR_PATH
    run cargo test --lib live_mutating_gateway_tests_are_explicitly_gated -- --nocapture
    ;;
  gateway-deposit)
    require_env_value RUN_LIVE_SOLANA_TESTS 1
    require_env_value RUN_LIVE_GATEWAY_TESTS 1
    require_env_value RUN_LIVE_GATEWAY_DEPOSIT_TESTS 1
    require_nonempty_env SOLANA_RPC_URL
    require_nonempty_env CIRCLE_GATEWAY_SOLANA_ADDRESS
    require_any_env "maker wallet" MAKER_PRIVATE_KEY MAKER_KEYPAIR_JSON MAKER_KEYPAIR_PATH
    require_tiny_gateway_deposit_cap
    run cargo test --lib live_gateway_deposit_usdc_skips_without_explicit_opt_in -- --nocapture
    ;;
  gateway-refill)
    require_env_value RUN_LIVE_SOLANA_TESTS 1
    require_env_value RUN_LIVE_GATEWAY_TESTS 1
    require_env_value RUN_LIVE_GATEWAY_REFILL_TESTS 1
    require_nonempty_env SOLANA_RPC_URL
    require_nonempty_env CIRCLE_GATEWAY_SOLANA_ADDRESS
    require_any_env "maker wallet" MAKER_PRIVATE_KEY MAKER_KEYPAIR_JSON MAKER_KEYPAIR_PATH
    require_tiny_gateway_refill_cap
    require_tiny_gateway_refill_fee_cap
    run cargo test --lib live_gateway_refill_usdc_skips_without_explicit_opt_in -- --nocapture
    ;;
  live-cbbtc-rfq)
    require_env_value RUN_LIVE_SOLANA_TESTS 1
    require_env_value RUN_LIVE_CBBTC_RFQ_TESTS 1
    require_nonempty_env SOLANA_RPC_URL
    require_nonempty_env JUPITER_API_KEY
    require_nonempty_env CIRCLE_GATEWAY_SOLANA_ADDRESS
    require_any_env "maker wallet" MAKER_PRIVATE_KEY MAKER_KEYPAIR_JSON MAKER_KEYPAIR_PATH
    require_any_env "taker wallet" TAKER_PRIVATE_KEY TAKER_KEYPAIR_JSON TAKER_KEYPAIR_PATH
    require_tiny_cbbtc_rfq_cap
    run cargo test --test live_mainnet live_cbbtc_rfq_accept_skips_without_explicit_opt_in -- --nocapture
    ;;
  live-e2e-gated)
    require_env_value RUN_LIVE_SOLANA_TESTS 1
    require_env_value RUN_LIVE_HTLC_TESTS 1
    require_env_value RUN_LIVE_JUPITER_SWAP_TESTS 1
    require_any_env "maker wallet" MAKER_PRIVATE_KEY MAKER_KEYPAIR_JSON MAKER_KEYPAIR_PATH
    require_any_env "taker wallet" TAKER_PRIVATE_KEY TAKER_KEYPAIR_JSON TAKER_KEYPAIR_PATH
    require_tiny_jupiter_swap_cap
    run cargo test --lib live_htlc_smoke_tests_skip_without_explicit_opt_in -- --nocapture
    run cargo test --lib jupiter_live_swap_skips_without_explicit_mutating_opt_in -- --nocapture
    ;;
  all-non-mutating)
    "$0" unit
    "$0" fake-e2e
    "$0" api
    "$0" solana
    "$0" jupiter-quote
    "$0" jupiter-cbbtc-quote
    "$0" htlc
    "$0" gateway-balance
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
