# Releasing Shello

The distributable product is the Rust Agent. The release workflow tests
and packages only <code>agent/</code>; deploy the web application separately through
Wrangler.

## Configuration

| Setting | Value |
| --- | --- |
| Worker name | `shello` |
| GitHub repository | `jiayx/shello` |
| Bootstrap repository variable | `BOOTSTRAP_GITHUB_REPOSITORY=jiayx/shello` |
| Agent executable | `shello-agent` |
| Diagnostic environment variable | `SHELLO_TRACE` |
| Nested-agent marker | `SHELLO_AGENT_ACTIVE` |
| Durable Object class | `TTYSession` |
| Durable Object binding | `TTY_SESSION` |

The bootstrap downloads Agent assets from the configured repository's latest release.
Each release must contain the platform binaries and `checksums.txt` listed below.
Browser viewer identity is stored per origin and session in `sessionStorage`.

## Versioning

The Agent package version in <code>agent/Cargo.toml</code> and the Git tag use the same
semantic version, with a <code>v</code> prefix on the tag:

~~~text
agent/Cargo.toml  <version>
Git tag           v<version>
~~~

Release notes describe the current version’s capabilities and behavior. The release
script reads the matching version section in `CHANGELOG.md`; each section uses an
`Added`, `Changed`, `Fixed`, `Removed`, or `Security` category heading. Development
history, migration narratives, and commit summaries do not belong in published notes.

## Pre-release checklist

From the repository root:

~~~bash
cd agent
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --release --all-targets
cargo build --locked --release
cd ..
pnpm build
git diff --check
git status --short
~~~

The working tree should contain only the intentional release changes. Verify that the
bootstrap configuration in <code>apps/web/wrangler.jsonc</code> points to the repository
whose release assets will be served.

## Prepare and publish

Prepare the next version:

~~~bash
./scripts/release.sh prepare 0.3.1
~~~

This updates <code>agent/Cargo.toml</code>, <code>agent/Cargo.lock</code>, and the
Agent README, then inserts an editable section in <code>CHANGELOG.md</code>. Replace
the TODO with current user-visible behavior and remove the
<code>release-draft</code> comment.

Publish after reviewing the resulting diff:

~~~bash
./scripts/release.sh publish 0.3.1
~~~

The publish command rejects unrelated working-tree changes and unfinished CHANGELOG
drafts. It tests both TLS feature sets, creates the release commit and annotated tag,
then pushes <code>main</code> and the tag. For a non-interactive invocation, run
<code>./scripts/release.sh publish 0.3.1 --yes</code>.

The <code>v*</code> tag triggers
<code>.github/workflows/build-agents.yml</code>. The workflow:

1. runs formatting, clippy, and release tests on Ubuntu;
2. runs native clippy and tests on macOS ARM64 and Windows AMD64;
3. builds six standard target binaries plus two portable Rustls Linux fallbacks; and
4. extracts the matching CHANGELOG section, generates <code>checksums.txt</code>,
   creates the GitHub Release, and uploads every binary.

Pull requests run only the first two steps. Direct <code>main</code> pushes do not
trigger this workflow, so a paired <code>main</code> + tag push creates exactly one
release run. Use <code>workflow_dispatch</code> for an explicit full-build check of a
branch; it does not publish a Release without a version tag.

Tag runs are not cancelled by the workflow's concurrency policy, so a release build is
allowed to finish even if a later commit is pushed.

## Release assets

Each GitHub Release includes:

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

The shell bootstrap consumes these exact names. It uses the portable Linux files only
when the standard system-TLS Agent fails its <code>--version</code> start probe. Do not
rename them without changing the Worker bootstrap manifest and scripts.

## Post-release verification

After the GitHub Action finishes, confirm that every binary and
<code>checksums.txt</code> is attached to the release, then test the deployed bootstrap:

~~~bash
curl -fsSL https://your-shello.example/start | sh
~~~

The script must identify the platform, validate the downloaded checksum, and print a
viewer URL after the Agent connects. Test a viewer connection, a rejected control
request, and an approved control request before announcing the release.
