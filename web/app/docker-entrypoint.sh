#!/bin/sh
set -eu

RUNTIME_ENV_PATH=/srv/app/runtime-env.js
SOLANA_RPC_URL=${FIRMAMENT_SOLANA_RPC_URL:-${VITE_SOLANA_RPC_URL:-https://api.mainnet-beta.solana.com}}

json_escape() {
    printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

cat > "$RUNTIME_ENV_PATH" <<EOF
window.__FIRMAMENT_CONFIG__ = Object.freeze({
  solanaRpcUrl: "$(json_escape "$SOLANA_RPC_URL")"
});
EOF

exec "$@"
