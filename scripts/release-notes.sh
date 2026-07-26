#!/bin/sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
RAW_VERSION=${1:-}
VERSION=${RAW_VERSION#v}
CHANGELOG=${2:-"$ROOT_DIR/CHANGELOG.md"}

if [ -z "$VERSION" ]; then
  echo "usage: $0 <version> [changelog]" >&2
  exit 2
fi

awk -v version="$VERSION" '
  index($0, "## [" version "]") == 1 {
    found = 1
    next
  }
  found && /^## \[/ {
    exit
  }
  found {
    print
  }
  END {
    if (!found) {
      exit 1
    }
  }
' "$CHANGELOG"
