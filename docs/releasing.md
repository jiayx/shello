# Deployment and releases

The web application deploys to Cloudflare Workers. Agent binaries are distributed
through GitHub Releases.

## Web deployment

Requires a Cloudflare account. From the repository root:

~~~bash
pnpm install
pnpm build
pnpm run deploy
~~~

Configuration lives in [wrangler.jsonc](../apps/web/wrangler.jsonc):

| Setting | Default |
| --- | --- |
| Worker | `shello` |
| Durable Object class / binding | `TTYSession` / `TTY_SESSION` |
| `BOOTSTRAP_GITHUB_REPOSITORY` | `jiayx/shello` |

The bootstrap uses the configured repository's latest release. Custom asset locations
use `BOOTSTRAP_BINARY_BASE_URL` and `BOOTSTRAP_CHECKSUMS_URL`.

## Agent releases

The package version in `agent/Cargo.toml` matches the Git tag without its `v` prefix.
Release notes come from the matching section in `CHANGELOG.md`, under `Added`,
`Changed`, `Fixed`, `Removed`, or `Security` headings.

~~~bash
./scripts/release.sh prepare 0.3.1
./scripts/release.sh publish 0.3.1
~~~

`prepare` updates version metadata and creates a draft CHANGELOG section. `publish`
requires completed notes without TODOs or the `release-draft` marker. It validates both
TLS backends, commits release files, and pushes `main` and an annotated version tag.
`--yes` skips the interactive confirmation.

## CI

[build-agents.yml](../.github/workflows/build-agents.yml) runs Rust checks on Ubuntu,
macOS ARM64, and Windows AMD64.

| Trigger | Result |
| --- | --- |
| Relevant pull request | Formatting, lint and tests |
| `v*` tag | Checks, platform builds, checksums and GitHub Release |
| `workflow_dispatch` on a branch | Checks and platform builds without publishing |
| Direct `main` push | No Agent workflow |

Portable builds use musl and Rustls. CI checks that those binaries run and contain no
dynamic loader or shared-library dependencies.

## Assets and bootstrap

Each release includes these files, whose names are used by the bootstrap:

~~~text
shello-agent-darwin-amd64
shello-agent-darwin-arm64
shello-agent-linux-amd64
shello-agent-linux-arm64
shello-agent-linux-amd64-portable
shello-agent-linux-arm64-portable
shello-agent-windows-amd64.exe
shello-agent-windows-arm64.exe
checksums.txt
~~~

`/start` and `/start.ps1` select the platform binary and verify SHA-256 before running
it. Each invocation uses an isolated temporary directory, removed after exit. Linux
tries the system-TLS binary first; if its `--version` probe fails, the script uses the
static musl/Rustls fallback, which needs neither glibc nor system OpenSSL.

Local development serves binaries from `apps/web/public/downloads/local/`, generated
by `./scripts/build-local-agent.sh`. Public sharing commands are in the
[README](../README.md#start-sharing); Agent checks are in the
[Agent reference](../agent/README.md#build-and-test).
