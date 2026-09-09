<p align="center">
  <img src="apps/web/public/logo.svg" alt="Shello logo" width="96" height="96">
</p>

<h1 align="center">Shello</h1>

<p align="center">One command. Share your shell.</p>

<p align="center">English · <a href="README.zh-CN.md">简体中文</a></p>

<p align="center"><a href="https://shello.tools.tf/">Try Shello</a></p>

Share your local shell through a browser link with one command. No installation or
inbound ports required. Available for macOS, Linux, and Windows.

## Start sharing

macOS and Linux:

~~~bash
curl -fsSL https://shello.tools.tf/start | sh
~~~

Windows PowerShell:

~~~powershell
irm 'https://shello.tools.tf/start.ps1' | iex
~~~

Send the printed URL to the other person. You can also create a session on
[Shello](https://shello.tools.tf/) and run the command shown there.

## Using Shello

- Viewers are read-only by default. The host clicks `[Y:allow]` / `[N:deny]` in the
  status bar to approve or reject a request. `Y` approves; `N`, Enter, Ctrl-C, or Escape rejects it. The viewer can release control from the
  toolbar; the host can revoke it by clicking the status bar’s “Click to revoke” action.
- A fixed status bar shows sharing and control status. Ordinary shells support
  scrollback; full-screen terminal apps use their own input modes.
- The browser scales the terminal to fit without changing the host's dimensions.
- Temporary disconnections reconnect automatically. Refreshing restores the current
  screen; complete scrollback is not guaranteed.
- Type `exit` to end sharing, or use Ctrl-D on Unix. The link can be reused within its
  lifetime, but previous programs are not restored.
- The web interface automatically selects Chinese or English from the browser language.

Anyone with the link can view the shared terminal, so keep it private. Remote control
requires host approval. Connections use the deployment's HTTPS; Shello does not provide
end-to-end encryption or user accounts.

## Local development

Requires Node.js, pnpm, and Rust stable.

~~~bash
pnpm install
pnpm dev
~~~

In another terminal:

~~~bash
./scripts/start.sh http://localhost:5173
~~~

On Windows, use `./scripts/start.ps1 -Server http://localhost:5173`.
To use the local website's download command, build a local binary with
`./scripts/build-local-agent.sh`.

## Documentation

| Document | Contents |
| --- | --- |
| [Agent reference](agent/README.md) | CLI options, builds, diagnostics |
| [Architecture](docs/architecture.md) | Terminal rendering, sessions, protocol |
| [Deployment and releases](docs/releasing.md) | Cloudflare configuration, Agent releases |
| [Release notes](CHANGELOG.md) | Version capabilities |

`apps/web` contains the React frontend and Cloudflare Worker; `agent` contains the Rust
host Agent; `scripts` contains development and release helpers.
