#!/bin/sh
set -eu

CONFIG_PATH=/app/config.toml
DATA_DIR=/data

fail() {
    printf '%s\n' "firmament container configuration error: $*" >&2
    exit 78
}

toml_value() {
    section=$1
    key=$2

    awk -v section="$section" -v key="$key" '
        {
            line = $0
            sub(/[[:space:]]*#.*/, "", line)
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", line)
        }
        /^[[:space:]]*\[/ {
            in_section = (line == "[" section "]")
            next
        }
        in_section {
            if (line ~ "^[[:space:]]*" key "[[:space:]]*=") {
                sub("^[^=]*=[[:space:]]*", "", line)
                gsub(/^[[:space:]]+|[[:space:]]+$/, "", line)
                gsub(/^"|"$/, "", line)
                print line
            }
        }
    ' "$CONFIG_PATH" | tail -n 1
}

[ -f "$CONFIG_PATH" ] || fail "mount config.toml to $CONFIG_PATH"
[ -r "$CONFIG_PATH" ] || fail "$CONFIG_PATH is not readable by the firmament user"
[ -d "$DATA_DIR" ] || fail "$DATA_DIR must exist for SQLite persistence"
[ -w "$DATA_DIR" ] || fail "$DATA_DIR is not writable by the firmament user"

bind_address=$(toml_value http bind_address)
[ -n "$bind_address" ] || fail "config.toml must set [http].bind_address"

case "$bind_address" in
    0.0.0.0|::)
        ;;
    *)
        fail "[http].bind_address must be 0.0.0.0 or :: inside the container, got '$bind_address'"
        ;;
esac

database_path=$(toml_value runtime database_path)
[ -n "$database_path" ] || fail "config.toml must set [runtime].database_path"

case "$database_path" in
    "$DATA_DIR"/*)
        ;;
    *)
        fail "[runtime].database_path must live under $DATA_DIR, got '$database_path'"
        ;;
esac

exec "$@"
