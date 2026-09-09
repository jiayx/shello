#!/bin/sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
MODE=${1:-}
RAW_VERSION=${2:-}
VERSION=${RAW_VERSION#v}
CONFIRM=${3:-}
TAG="v$VERSION"
PACKAGE_FILE="$ROOT_DIR/agent/Cargo.toml"
AGENT_README="$ROOT_DIR/agent/README.md"
CHANGELOG="$ROOT_DIR/CHANGELOG.md"

usage() {
  cat >&2 <<EOF
usage:
  $0 prepare <version>
  $0 publish <version> [--yes]

prepare  bumps the Agent version and creates an editable CHANGELOG draft.
publish  validates the draft, tests both TLS backends, commits, tags, and pushes.
EOF
  exit 2
}

fail() {
  echo "release: $*" >&2
  exit 1
}

current_version() {
  awk '
    /^\[package\]$/ { package = 1; next }
    /^\[/ { package = 0 }
    package && /^version = "/ {
      gsub(/^version = "|".*$/, "")
      print
      exit
    }
  ' "$PACKAGE_FILE"
}

require_main() {
  branch=$(git -C "$ROOT_DIR" branch --show-current)
  [ "$branch" = "main" ] || fail "current branch is '$branch'; releases must start from main"
}

require_version() {
  [ -n "$VERSION" ] || usage
  printf '%s\n' "$VERSION" |
    grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$' ||
    fail "invalid semantic version: $VERSION"
}

require_tag_absent() {
  if git -C "$ROOT_DIR" rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    fail "tag already exists locally: $TAG"
  fi
  if git -C "$ROOT_DIR" ls-remote --exit-code --tags origin "refs/tags/$TAG" >/dev/null 2>&1; then
    fail "tag already exists on origin: $TAG"
  fi
}

prepare() {
  require_main
  [ -z "$(git -C "$ROOT_DIR" status --porcelain)" ] ||
    fail "working tree must be clean before prepare"
  require_tag_absent

  old_version=$(current_version)
  [ "$old_version" != "$VERSION" ] || fail "Agent is already version $VERSION"
  ! grep -q "^## \[$VERSION\]" "$CHANGELOG" ||
    fail "CHANGELOG already contains version $VERSION"

  temp_dir=$(mktemp -d "${TMPDIR:-/tmp}/shello-release.XXXXXX")
  trap 'rm -rf "$temp_dir"' EXIT HUP INT TERM

  awk -v version="$VERSION" '
    /^\[package\]$/ { package = 1 }
    /^\[/ && $0 != "[package]" { package = 0 }
    package && /^version = "/ && !updated {
      print "version = \"" version "\""
      updated = 1
      next
    }
    { print }
    END { if (!updated) exit 1 }
  ' "$PACKAGE_FILE" > "$temp_dir/Cargo.toml"
  mv "$temp_dir/Cargo.toml" "$PACKAGE_FILE"

  awk -v version="$VERSION" '
    /^# shello-agent / && !updated {
      print "# shello-agent " version
      updated = 1
      next
    }
    { print }
    END { if (!updated) exit 1 }
  ' "$AGENT_README" > "$temp_dir/agent-README.md"
  mv "$temp_dir/agent-README.md" "$AGENT_README"

  latest_tag=$(git -C "$ROOT_DIR" describe --tags --match 'v*' --abbrev=0 2>/dev/null || true)
  {
    printf '## [%s] - %s\n\n' "$VERSION" "$(date +%Y-%m-%d)"
    printf '### Changed\n\n'
    printf '%s\n\n' '- TODO: replace this line with user-visible changes.'
    printf '%s\n' '<!-- release-draft'
    if [ -n "$latest_tag" ]; then
      printf 'Commit summary since %s:\n' "$latest_tag"
      git -C "$ROOT_DIR" log --reverse --format='- `%h` %s' "$latest_tag..HEAD"
    else
      printf '%s\n' 'Commit summary:'
      git -C "$ROOT_DIR" log --reverse --format='- `%h` %s'
    fi
    printf '%s\n' 'Rewrite the summary above and remove this comment before publishing.'
    printf '%s\n' '-->'
  } > "$temp_dir/draft.md"

  awk -v draft="$temp_dir/draft.md" '
    { print }
    $0 == "## [Unreleased]" {
      print ""
      while ((getline line < draft) > 0) {
        print line
      }
      close(draft)
    }
  ' "$CHANGELOG" > "$temp_dir/CHANGELOG.md"
  mv "$temp_dir/CHANGELOG.md" "$CHANGELOG"

  (
    cd "$ROOT_DIR/agent"
    cargo check
  )

  echo
  echo "Prepared $TAG from $old_version."
  echo "Edit CHANGELOG.md into user-facing Added/Changed/Fixed entries,"
  echo "remove TODO and the release-draft comment, then run:"
  echo "  ./scripts/release.sh publish $VERSION"
}

publish() {
  require_main
  require_tag_absent
  [ "$(current_version)" = "$VERSION" ] ||
    fail "agent/Cargo.toml does not contain version $VERSION"
  grep -q "^## \[$VERSION\] - " "$CHANGELOG" ||
    fail "CHANGELOG does not contain a dated $VERSION section"

  temp_dir=$(mktemp -d "${TMPDIR:-/tmp}/shello-release.XXXXXX")
  trap 'rm -rf "$temp_dir"' EXIT HUP INT TERM
  "$ROOT_DIR/scripts/release-notes.sh" "$VERSION" > "$temp_dir/release-notes.md" ||
    fail "could not extract CHANGELOG section for $VERSION"
  if grep -Eq 'TODO|release-draft' "$temp_dir/release-notes.md"; then
    fail "CHANGELOG $VERSION section still contains a draft marker"
  fi
  grep -Eq '^### (Added|Changed|Fixed|Removed|Security)' "$temp_dir/release-notes.md" ||
    fail "CHANGELOG $VERSION section has no user-visible category"

  unexpected=$(
    git -C "$ROOT_DIR" status --porcelain |
      awk '
        {
          path = substr($0, 4)
          if (path != "CHANGELOG.md" &&
              path != "agent/Cargo.toml" &&
              path != "agent/Cargo.lock" &&
              path != "agent/README.md") {
            print path
          }
        }
      '
  )
  if [ -n "$unexpected" ]; then
    echo "release: unexpected release changes:" >&2
    echo "$unexpected" >&2
    exit 1
  fi

  git -C "$ROOT_DIR" fetch --quiet origin main
  [ "$(git -C "$ROOT_DIR" rev-parse HEAD)" = "$(git -C "$ROOT_DIR" rev-parse origin/main)" ] ||
    fail "local main is not aligned with origin/main"

  (
    cd "$ROOT_DIR/agent"
    cargo fmt --check
    cargo clippy --locked --all-targets -- -D warnings
    cargo test --locked --release --all-targets
    cargo clippy --locked --no-default-features --features rustls-tls --all-targets -- -D warnings
    cargo test --locked --no-default-features --features rustls-tls --release --all-targets
  )
  git -C "$ROOT_DIR" diff --check

  echo
  "$ROOT_DIR/scripts/release-notes.sh" "$VERSION"
  echo
  echo "Ready to commit, tag, and push $TAG."
  if [ "$CONFIRM" != "--yes" ]; then
    printf 'Continue? [y/N] '
    read -r answer
    case "$answer" in
      y|Y|yes|YES) ;;
      *) fail "cancelled" ;;
    esac
  fi

  git -C "$ROOT_DIR" add \
    CHANGELOG.md \
    agent/Cargo.toml \
    agent/Cargo.lock \
    agent/README.md
  git -C "$ROOT_DIR" commit -m "release: $TAG"
  git -C "$ROOT_DIR" tag -a "$TAG" -m "$TAG"
  git -C "$ROOT_DIR" push origin main
  git -C "$ROOT_DIR" push origin "$TAG"

  echo "Published $TAG. GitHub Actions will create the Release and upload its assets."
}

require_version
case "$MODE" in
  prepare) prepare ;;
  publish) publish ;;
  *) usage ;;
esac
