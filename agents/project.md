# async-rust 项目索引
> 最后更新：2026-09-08

## 项目目标
独立公开的本地课堂助手。无需注册、登录、邀请码；全部 BYOK。

## 技术栈
计划使用 Tauri 2 + Rust + Vue，复用 toolkit model-auth 与完整 Fluent 样式和控件。参考 Async_Pub 的课堂业务及 SynthV Toolbox 的桌面宿主实现。

## 模块结构
当前只有 README 和 agents 调研记录，尚未实施桌面代码。新增测试统一放在根目录 test。

## 产品约束
用户凭据由本地宿主保存，模型请求使用用户自己的供应商。不得引入服务端账户或邀请码门槛。源码调查见任务目录。
