# Findings

- 当前专属 worktree 从 `origin/main` 创建，前端与 Rust 文件由其他协作者后续提供；工作流需依赖根目录 `package.json` 和 `src-tauri/Cargo.toml` 的约定。
- Toolbox 工作流含 Python、SynthV Bridge、FFmpeg、专有组件及 release notes 逻辑；Async 桌面工作流只保留 npm、Rust、Tauri 构建和无签名 release 资产。
