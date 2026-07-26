# Releasing ttys

The distributable product is the Rust Agent. The release workflow intentionally tests
and packages only <code>agent/</code>; deploy the web application separately through
Wrangler.

## Versioning

The Agent package version in <code>agent/Cargo.toml</code> and the Git tag use the same
semantic version, with a <code>v</code> prefix on the tag. For this release:

~~~text
agent/Cargo.toml  0.2.0
Git tag           v0.2.0
~~~

Record user-visible changes in <code>CHANGELOG.md</code> before creating the tag.

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

## Publish flow

Commit the version and documentation changes, then create and push an annotated tag:

~~~bash
git add README.md CHANGELOG.md docs agent/Cargo.toml agent/Cargo.lock
git commit -m "release: v0.2.0"
git tag -a v0.2.0 -m "v0.2.0"
git push origin main --follow-tags
~~~

The <code>v*</code> tag triggers
<code>.github/workflows/build-agents.yml</code>. The workflow:

1. runs formatting, clippy, and release tests on Ubuntu;
2. runs native clippy and tests on macOS ARM64 and Windows AMD64;
3. builds six optimized target binaries; and
4. generates <code>checksums.txt</code> and publishes the GitHub Release.

Pull requests run only the first two steps. Direct <code>main</code> pushes do not
trigger this workflow, so a paired <code>main</code> + tag push creates exactly one
release run. Use <code>workflow_dispatch</code> for an explicit manual check of a
branch.

Tag runs are not cancelled by the workflow's concurrency policy, so a release build is
allowed to finish even if a later commit is pushed.

## Release assets

Each GitHub Release includes:

~~~text
ttys-agent-darwin-amd64
ttys-agent-darwin-arm64
ttys-agent-linux-amd64
ttys-agent-linux-arm64
ttys-agent-windows-amd64.exe
ttys-agent-windows-arm64.exe
checksums.txt
~~~

The shell and PowerShell bootstrap scripts consume these exact names. Do not rename
them without changing the Worker bootstrap manifest and scripts.

## Post-release verification

After the GitHub Action finishes, confirm that every binary and
<code>checksums.txt</code> is attached to the release, then test the deployed bootstrap:

~~~bash
curl -fsSL https://your-ttys.example/start | sh
~~~

The script must identify the platform, validate the downloaded checksum, and print a
viewer URL after the Agent connects. Test a viewer connection, a rejected control
request, and an approved control request before announcing the release.
