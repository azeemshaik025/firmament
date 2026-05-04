#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

CARGO_BIN="${CARGO:-cargo}"
WARMUP_SECONDS="${FIRMAMENT_WARMUP_SECONDS:-5}"
LOG_DIR="${FIRMAMENT_LOG_DIR:-$ROOT_DIR/target/firmament-demo}"
MAKER_LOG="$LOG_DIR/maker-runtime.log"
MAKER_PID=""

cleanup() {
  local status=$?
  trap - EXIT INT TERM

  if [[ -n "$MAKER_PID" ]] && kill -0 "$MAKER_PID" 2>/dev/null; then
    printf '\nStopping maker runtime (pid %s)...\n' "$MAKER_PID"
    kill "$MAKER_PID" 2>/dev/null || true
    wait "$MAKER_PID" 2>/dev/null || true
  fi

  exit "$status"
}

trap cleanup EXIT INT TERM

if [[ ! "$WARMUP_SECONDS" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
  printf 'FIRMAMENT_WARMUP_SECONDS must be a number, got: %s\n' "$WARMUP_SECONDS" >&2
  exit 2
fi

mkdir -p "$LOG_DIR"
printf 'Building maker runtime...\n'
"$CARGO_BIN" build --release --bins

printf 'Starting maker runtime in the background...\n'
printf 'Maker log: %s\n' "$MAKER_LOG"
"$ROOT_DIR/target/release/firmament" >"$MAKER_LOG" 2>&1 &
MAKER_PID=$!

printf 'Waiting %s seconds for maker runtime warmup...\n' "$WARMUP_SECONDS"
sleep "$WARMUP_SECONDS"

if ! kill -0 "$MAKER_PID" 2>/dev/null; then
  printf 'Maker runtime exited during warmup. Recent log output:\n' >&2
  tail -n 80 "$MAKER_LOG" >&2 || true
  exit 1
fi

printf 'Maker runtime is ready.\n'
printf 'Open http://127.0.0.1:5050/app for swaps or http://127.0.0.1:5050/app/admin for the admin cockpit.\n'
printf 'Use the HTTP API directly under /v1 for scripted demo operations.\n'
