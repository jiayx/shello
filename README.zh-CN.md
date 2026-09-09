<p align="center">
  <img src="apps/web/public/logo.svg" alt="Shello logo" width="96" height="96">
</p>

<h1 align="center">Shello</h1>

<p align="center">一条命令，共享本地命令行。</p>

<p align="center"><a href="README.md">English</a> · 简体中文</p>

<p align="center"><a href="https://shello.tools.tf/">在线体验</a></p>

通过浏览器链接与他人共享本地 shell，无需手动安装或开放入站端口。
支持 macOS、Linux 和 Windows。

## 开始共享

macOS 和 Linux：

~~~bash
curl -fsSL https://shello.tools.tf/start | sh
~~~

Windows PowerShell：

~~~powershell
irm 'https://shello.tools.tf/start.ps1' | iex
~~~

将终端输出的链接发给对方即可。也可以先在 [Shello](https://shello.tools.tf/)
创建会话，再执行页面提供的命令。

## 使用说明

- 访问者默认只读。主机可点击状态栏的 `[Y:allow]` / `[N:deny]` 批准或拒绝控制请求，
  也可按 `Y` 批准，按 `N`、Enter、Ctrl-C 或 Escape 拒绝。
  控制方可通过网页工具栏释放控制；主机可点击状态栏的 “Click to revoke” 撤销控制。
- 主机底部固定状态栏显示共享与控制状态。普通 shell 支持滚动历史，全屏终端应用使用自身的输入模式。
- 网页按主机终端尺寸等比缩放，不会改变主机的行列数。
- 短暂断线会自动重连。刷新网页可恢复当前画面，不保证恢复完整滚动历史。
- 输入 `exit` 结束共享，Unix 也可使用 Ctrl-D。链接在有效期内可重复使用，但不会恢复之前运行的程序。
- 网页根据浏览器语言自动选择中文或英文。

持有链接的人都能查看共享终端，请妥善保管链接。远程控制需要主机批准。
连接使用部署站点的 HTTPS；Shello 不提供端到端加密或用户账号。

## 本地开发

需要 Node.js、pnpm 和 Rust stable。

~~~bash
pnpm install
pnpm dev
~~~

在另一个终端启动 Agent：

~~~bash
./scripts/start.sh http://localhost:5173
~~~

Windows 使用 `./scripts/start.ps1 -Server http://localhost:5173`。
如需使用本地网站提供的下载命令，先运行 `./scripts/build-local-agent.sh` 构建本地二进制文件。

## 文档

以下技术文档为英文。

| 文档 | 内容 |
| --- | --- |
| [Agent 参考](agent/README.md) | 命令行选项、构建、诊断 |
| [架构](docs/architecture.md) | 终端渲染、会话、协议 |
| [部署与发布](docs/releasing.md) | Cloudflare 配置、Agent 发布 |
| [版本说明](CHANGELOG.md) | 各版本功能 |

`apps/web` 包含 React 前端和 Cloudflare Worker，`agent` 包含 Rust 主机 Agent，
`scripts` 包含开发和发布脚本。
