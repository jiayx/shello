#!/bin/sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SERVER_URL=${1:-http://localhost:5173}
SESSION_ID=${2:-}

cd "$ROOT_DIR/agent"

if [ -n "$SESSION_ID" ]; then
  exec cargo run -- -server "$SERVER_URL" -session "$SESSION_ID"
fi

exec cargo run -- -server "$SERVER_URL"
