# vt100 terminal parser

[vt100 0.16.2](https://github.com/doy/vt100-rust), licensed under [MIT](LICENSE).

## 为什么放在 vendor 中

Shello 使用 vt100 解析子 shell 的输出，在主机终端的固定状态栏上方重绘内容，
并为网页刷新、重连和网络拥堵恢复生成当前画面的快照。

上游 0.16.2 的清屏历史行为和快照接口无法直接满足这些需求，相关修改涉及内部的
屏幕缓冲区、滚动区域和解析回调。因此 Agent 通过 Cargo 路径依赖使用这份源码，
将所需补丁随项目一起维护。上游的 MIT 许可及版权声明保留在 [LICENSE](LICENSE) 中。

## 相对上游 0.16.2 的改动

| 文件 | 改动 |
| --- | --- |
| [src/grid.rs](src/grid.rs)、[src/screen.rs](src/screen.rs) | `CSI 2 J` 清屏时，在启用历史且没有局部滚动区域的情况下，将有内容的屏幕行保留到滚动历史，遵守历史容量限制；`CSI 3 J` 清空历史并重置滚动偏移，不清除当前画面。 |
| [src/screen.rs](src/screen.rs)、[src/grid.rs](src/grid.rs) | 新增 `Screen::snapshot_formatted()`，输出重建当前画面的终端序列。全屏应用运行时同时重建主缓冲区和备用缓冲区，并恢复光标、样式、输入模式、滚动区域及原点模式。快照不包含滚动历史，重置序列仅用于接收快照的终端。 |
| [src/parser.rs](src/parser.rs)、[src/perform.rs](src/perform.rs) | 新增 `Parser::process_for_snapshot()` 和 `snapshot_ready()`，通过解析回调跟踪 UTF-8 字符及控制序列是否完整，避免快照截断字符或转义序列。普通 `process()` 接口保持不变。 |

补丁适用于普通 shell 和全屏 TUI，没有针对 OpenCode 等具体应用的分支。
窗口缩放时的文本重排（reflow）未实现，缩窄后再展开仍可能丢失被截断的内容。
