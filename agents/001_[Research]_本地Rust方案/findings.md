# 调查与决策

## 现状证据
- Async main 起点为 4ca85ee，调查前工作区干净，只有一个 worktree。
- 根 README 的预留后端描述已经落后于代码：api_routes.py 已有工作区、目录、材料上传、会话、消息和摘要接口；session_chat.py 已有工具调用和摘要任务。
- SynthV Toolbox 的实际桌面根在 src/PiDesktop.Tauri，使用 Tauri 2、Rust、Tokio、reqwest、keyring，并消费 platform-kit 发布的 model-auth 归档。
- platform-kit/kits/model-auth 包含 core、providers、vue；宿主负责持久化、网络授权、供应商可用性和模型选择。当前本地 toolkit 与 Toolbox 使用的发布归档版本不同，安装前必须核对发布产物，不能直接认定本地目录最新。
- 本地 styles/fluent 导出主题、按钮、输入、开关、滑块、选择、提示、弹窗、弹出层和导航。没有发现名为 full 的入口；当前清单不足以证明所有标准控件已齐全。
- Async 的 auth.py 是用户账户、密码、令牌和邀请逻辑，与模型供应商授权不同。

## 建议架构
保留 Vue 工作台业务；Tauri 2 提供桌面宿主，Rust 承担本地存储、文件访问、模型网络请求、授权回调、凭据保存和后台任务。建议 SQLite 保存结构化数据，应用数据目录保存材料文件。采用命令调用与流式 Channel，避免为内部界面保留 HTTP 服务。

授权界面直接消费 toolkit Vue 组件，Rust 实现宿主适配器；通用授权能力若需提取，应回到 toolkit，而非复制整个 Toolbox。以 Fluent Vue 控件和完整样式替换实际使用的旧控件，缺失通用控件在 toolkit 补齐。

本地单用户产品建议取消服务器账户与邀请码门槛；这是方案建议，尚未实施。模型授权不能直接替代用户账户。语音识别供应商仍需独立验证，不能因聊天模型授权完成而认定 ASR 可用。

## 实施顺序与验收
1. 桌面启动、工作区和材料读写、重启恢复。
2. 授权保存、重启加载、模型列表、流式问答和取消。
3. 音频采集/导入、转写、摘要及材料检索的完整课堂流程。
4. 全部使用中的 Fluent 控件交互、主题、键盘与浮层验证，以及安装包运行。
被完整替换的旧服务模块再删除；最终不依赖 Python 后端，不增加旧 API 兼容层。

## 外部依据
Tauri 官方文档 https://v2.tauri.app/develop/calling-rust/ 推荐 Channel 传递流式 HTTP 数据；https://v2.tauri.app/concept/inter-process-communication/ 说明 invoke 调用 Rust 命令。

## 调查错误
- 猜测 Toolbox 根目录存在 package.json -> 文件不存在 -> 通过文件清单定位实际桌面子目录。
- model-auth 文件通配无匹配 -> 不据此推断无集成；package.json 已证实相关依赖。

本轮未构建、未运行桌面程序，结论属于源码调查，不能作为运行验收。

## 用户明确决定
本地版无需邀请码和登录，全部 BYOK。独立公开仓库为 lsy-404/async-rust。
