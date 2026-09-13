# Async

本地优先的课堂助手，采用 Tauri 2、Rust 和 Vue，使用 Platform Kit 的模型授权界面与 Fluent 控件。

## 项目概览

Async 是一个桌面端课堂工作台，支持：

- 工作区、会话和材料管理
- 连接兼容 OpenAI API 的模型供应商，可配置自定义 HTTP(S) 地址与 API Key
- 从 `/v1/models` 获取模型列表并选择聊天模型
- 基于会话内容、课堂材料和转写内容进行流式对话与摘要
- 导入 TXT、Markdown、DOCX 材料
- 导入音频或直接录音后转写
- 数据本地保存到 Tauri 宿主管理的 SQLite 数据库，API Key 保存到系统凭据库

模型请求会发送给你选择的供应商；请只使用你有权限处理的课堂内容和账户。

## 本地开发

需要 Node.js 22.12+、Rust stable 和对应系统的 [Tauri 开发依赖](https://v2.tauri.app/start/prerequisites/)。

```sh
npm ci
npm run tauri dev
```

`npm run dev` 只启动前端开发服务。文件、凭据和模型操作通过 Tauri 的 Rust 宿主执行，请使用桌面程序完成工作。

## 常用脚本

```sh
npm run build
npm run typecheck
npm test
cargo test --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
npm run tauri build
```

## 仓库文档

- [TOS.md](TOS.md)：使用条款与免责声明
- [NOTICE.md](NOTICE.md)：第三方来源说明
- [LICENSE](LICENSE)：项目源码许可证

## 许可证

AGPL-3.0-or-later。第三方来源见 [NOTICE.md](NOTICE.md)。
