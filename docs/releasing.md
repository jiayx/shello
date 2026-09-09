# Releasing Shello

The distributable product is the Rust Agent. The release workflow intentionally tests
and packages only <code>agent/</code>; deploy the web application separately through
Wrangler.

## Shello naming transition

The CLI and release assets now use `shello-agent`; environment variables use
`SHELLO_TRACE` and `SHELLO_AGENT_ACTIVE`. Publish a release containing the renamed
assets before deploying the updated bootstrap. Older releases only contain the old
asset names and cannot satisfy the new download requests.

The GitHub repository is `jiayx/shello`. `BOOTSTRAP_GITHUB_REPOSITORY` and the
local Git remote both point to that repository, which hosts the Agent releases.

The default Worker name is now `shello`. This targets a separate deployment from the
old Worker; it does not rename the deployed service or transfer its active sessions.
To update the existing deployment in place, retain its deployed Worker name during
the transition. Coordinate routes and domains when switching to the new deployment.
The `TTYSession` class, `TTY_SESSION` binding, and migration history retain their
technical identifiers so an in-place update does not replace the session namespace.

The browser reads the legacy viewer-token key when needed and stores it under the
new `shello.viewerToken.*` key, preserving identity on the same origin. A new origin
has separate browser storage and cannot inherit an active viewer's authorization.

## Versioning

The Agent package version in <code>agent/Cargo.toml</code> and the Git tag use the same
semantic version, with a <code>v</code> prefix on the tag:

~~~text
agent/Cargo.toml  <version>
Git tag           v<version>
~~~

The release script creates a draft from commits since the latest version tag. Commit
subjects are only source material: rewrite them into user-visible
<code>Added</code>, <code>Changed</code>, <code>Fixed</code>,
<code>Removed</code>, or <code>Security</code> entries before publishing.

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
./scripts/release.sh prepare 0.2.3
~~~

This updates <code>agent/Cargo.toml</code>, <code>agent/Cargo.lock</code>, and the
Agent README, then inserts an editable section in <code>CHANGELOG.md</code>. Replace
the TODO with concise user-visible changes and remove the
<code>release-draft</code> comment.

Publish after reviewing the resulting diff:

~~~bash
./scripts/release.sh publish 0.2.3
~~~

The publish command rejects unrelated working-tree changes and unfinished CHANGELOG
drafts. It tests both TLS feature sets, creates the release commit and annotated tag,
then pushes <code>main</code> and the tag. For a non-interactive invocation, run
<code>./scripts/release.sh publish 0.2.3 --yes</code>.

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
