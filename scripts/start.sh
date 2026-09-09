#!/bin/sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SERVER_URL=${1:-http://localhost:5173}
SESSION_ID=${2:-}

if [ -n "$SESSION_ID" ]; then
  exec cargo run --manifest-path "$ROOT_DIR/agent/Cargo.toml" -- -server "$SERVER_URL" -session "$SESSION_ID"
fi

exec cargo run --manifest-path "$ROOT_DIR/agent/Cargo.toml" -- -server "$SERVER_URL"
