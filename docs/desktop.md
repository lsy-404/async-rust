# 桌面使用

Async 无需账号登录或邀请码。界面使用 Vue 和 Platform Kit；OAuth、网络请求、音频识别和本地数据管理全部在 Rust 中执行，安装后无需 Node、Python 或独立后台服务。

## 开始使用

1. 新建工作区和课堂会话，导入 TXT、Markdown 或 DOCX 材料。
2. 在设置中选择“连接模型”：使用自己的 OpenAI 兼容 API Key，或通过 WorkBuddy、TraeCode 的供应商 OAuth 授权。选择聊天模型后保存设置。
3. 在设置中下载本地语音模型。模型包约 117 MB，下载完成并校验后即可离线转写。
4. 导入音频或录制麦克风，生成本地转写；随后可根据转写、材料和聊天记录提问或生成摘要。

界面主题和界面语言在顶部标题栏切换，中英双语，选择后立即生效并随其余设置保存在本地。侧栏支持搜索、工作区折叠、右键菜单和双击重命名，`Ctrl`/`Cmd` + `B` 可收起侧栏。

本地 STT 使用与 IRIS 相同的 Whisper tiny 多语言 INT8 和 Silero VAD 模型、哈希及推理配置，通过 Sherpa ONNX Rust 绑定静态链接运行。录音以 PCM WAV 传给 Rust，44.1/48 kHz 音频在本地重采样。取消识别在当前原生推理片段结束后生效，取消的结果不会保存。

API Key 与 OAuth 凭据明文存入应用私有目录的凭据文件，采用原子写入和文件锁；Unix 文件权限为 0600。不使用系统钥匙串或 safeStorage。工作区、会话、材料、转写和摘要保存在应用数据目录的 SQLite 中。问答和摘要会把相关文本提交给用户选定的模型供应商；语音识别本身不上传音频。

## 开发和检查

安装 Node.js 22.12+、Rust stable 和系统 Tauri 开发依赖后运行：

```sh
npm ci
npm run tauri dev
```

```sh
npm run build
npm test
cargo test --locked --manifest-path src-tauri/Cargo.toml --all-targets
cargo clippy --locked --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
npm run tauri build
```

真实语音模型验收测试需要下载公开模型与音频夹具：

```sh
cargo test --manifest-path src-tauri/Cargo.toml actual_whisper_jfk -- --ignored --nocapture
```

GitHub Actions 沿用 Toolbox 的准备与桌面构建分工，生成 Windows x64 NSIS 和 macOS Universal 安装包。`v*` 标签会核对 manifest 版本后发布构建产物。
