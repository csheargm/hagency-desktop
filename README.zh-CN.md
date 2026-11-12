# Hagency Desktop

0.5.0 已准备但尚未正式发布：[发布检查报告](dev-vibe/release-0.5.0/assessment.zh.md) 记录了待处理的上游依赖安全项。已发布版本见 [GitHub Releases](https://github.com/hagency-org/hagency-desktop/releases)。

[English](README.md) | 简体中文

Hagency Desktop 是原生 Rust/Makepad 客户端，将 Matrix 聊天、Project 工作区和
本机 AI Agent 管理整合到同一个应用。当前登录只支持 **Hagency 服务器**：填写
服务器地址，通过它的 Pasion 服务在浏览器登录。普通 Matrix 服务器不能作为
当前登录入口。

## 管理边界

- 一个 Project 绑定一个 Matrix Space；讨论 Room 各自维护成员。
- Agent 是服务器级身份，由创建者永久拥有。新建时默认绑定当前已授权设备，
  接入 Room 是独立操作，不要求先选 Project。
- 服务器校验身份、Room 接入权和执行设备。Project/Room 管理员设置 Agent
  接入权限与服务暂停。
- 主人在自己的设备上选择 Codex 或 Claude Code、模型、工作目录、token 配额和工具策略，
  无需服务器管理员审批资源消耗。
- 一台设备可以执行多个 Agent。切换账号会停止当前主人的本地运行，不转让
  Agent，也不合并账号数据。

## 快速开始

1. 输入 Hagency 服务器地址，在浏览器完成 Pasion 登录。这一次授权同时建立
   Matrix 聊天会话和原生主人管理会话。
2. 打开 **Projects → New Project**，或使用 Projects 右侧 **⋯**。默认创建新
   Space，也可以选择已经加入的 Space；从详情添加讨论 Room。
3. 打开 **Agents → New Agent**，填写名称，等待身份及主人私聊就绪。创建不带
   Project/Room，执行设备默认为当前已授权设备。
4. 在 **Model & resources** 连接本机已登录的 Codex 或 Claude Code，保存模型和
   推理强度。Hagency 会创建私有工作目录。
5. 在 **Usage & limits** 设置 Agent、Room 和请求者策略，再到 **Service**
   选择私聊或已绑定 Room 并启动。任务结束后按实际用量计费，已开始的任务
   可以超额，额度不足不会打断正在进行的工作。

详见[原生快速开始指南](docs/hagency-quickstart.zh-CN.md)。界面语言可切换；
上面的英文名称对应当前源码控件，部分管理页文字尚未完成中文翻译。

Project 详情 tab 为 **Overview / Rooms / Members / Access**；Agent 详情为
**Agent and device / Model & resources / Usage & limits / Service**，分别管理身份与设备、本机资源、额度与请求
策略，以及选中 Room 的运行与审批。

## 导航与保留功能

桌面侧栏提供 **Projects、Agents、Chats、Contacts、Mini apps**；前三项右侧
**⋯** 提供创建入口。**More** 中保留 Discover、Explore rooms、Moments、File
Transfer、Article editor、Browser、Settings。Project 条目的 **⋯** 可打开概览、
讨论 Room、成员、邀请成员以及绑定 Space；邀请按钮遵循真实 Matrix 权限。

聊天、联系人、搜索、附件、普通用户加密聊天、朋友圈、原生文章编辑器以及
小程序导入／权限审阅／运行均保留。默认小程序目录不再提供 Palpo 应用。
Hagency 管理由原生 Projects/Agents 页面承担，旧资源与协调者审批流程已移除。
Agent 可以读取加密 Room 历史，并通过当前授权的 Matrix 会话收发文件。
共享 Agent 每个请求最多自动发送 64 个文件、合计 64 MiB；超出时文件工具返回
错误，任务本身继续运行。

Codex 和 Claude 原生 session 会在轮次间保留上下文。macOS 的共享 Claude 使用
独立工作目录和 Seatbelt 隔离，不加载主人的 hooks、设置或无关 MCP 服务。
停止进程后原生会话文件加密保存，重新打开前须通过当前范围授权。
运行时要求 Claude Code 2.1.322 及以上、Codex 0.167.0 及以上，更低版本会被拒绝。
对话控制允许发送者身份已认证但未交叉签名的 Matrix 设备，仍校验卡片允许的
答复者和当前 Room 成员资格；这不授予工具审批权限。

Matrix Room、别名和消息链接在应用内打开；未加入的 Room 先显示加入确认。
消息链接定位到原 Room 或线程，聊天中的用户链接仍打开资料面板。详见
[Matrix 链接导航](docs/matrix-links.md)。

## 账号、设置与本地数据

通过 **Settings → Account** 的账号列表添加或切换账号；左下角账号菜单可打开
账号设置和退出登录。每个账号保留独立 Matrix 设备、加密数据库及主人配置，
只有当前账号同步。切换停止原账号 Agent 运行；退出会停止运行并撤销当前授权。

设置分为 **Account、Preferences、Privacy、About**；外观与语言在 Preferences。
通用外部 **Agent Access** 工具与原生 Hagency Agents 管理不同，配置 Hagency
不需要填写 BotFather 或 loopback 服务地址。

`HAGENCY_DESKTOP_DATA_DIR` 可指定绝对路径以隔离数据。启动器仍识别
`RINX_DATA_DIR`、`ROBRIX_DATA_DIR` 环境别名，但这不代表兼容已退休 Fleet 数据。
账本、工作目录及提供方关联按完整授权身份隔离。换执行设备不会转让所有权。
本次原生 CLI 切换要求配套的当前 Desktop、daemon 和服务器，不恢复旧版本运行
选项或审批协议。旧的运行意图会被丢弃，需要重新明确启动各个 Room。

设备验证、恢复密钥设置及恢复入口位于 **Settings → Account → Encryption and recovery**。
已验证的设备读取加密历史仍需房间密钥。见[设备验证与密钥恢复](docs/device-verification-recovery.md)。

## 构建与开发

主人引擎作为本仓库的 Rust 库维护于 [`crates/hagency-client`](crates/hagency-client/README.md)，无需单独检出 Client 或构建旧网页 UI。安装 CMake 和平台依赖后执行：

```sh
.jit/sources.sh    # 获取固定版本的 makepad 检出；.jit/sources.lock 变化后需重新执行
cargo run --locked --bin rinx
cargo test --locked --lib
```

应用基于 `.sources/` 下的一份 makepad 检出构建（已被 git 忽略，版本固定在
`.jit/sources.lock`）；在 `.jit/sources.sh` 获取它之前，任何 cargo 命令都无法运行。
同一份源码也可在 makepad JIT 运行器（`makepad run`）下运行，修改会直接注入正在运行的应用。
其安装、命令、内存需求与已知差距见 [`.jit/NOTES.md`](.jit/NOTES.md)（英文）。

仓库固定 Rust 1.98.0，Cargo 包／二进制仍名为 `rinx`，应用显示为 Hagency。
默认 standalone 构建已包含原生管理，不需要开启旧 `agent_chat` 仪器功能。
移动打包需要签名及实机验证。

内置应用目录是 `system-apps.json`，文章编辑器位于 `apps/article-editor/`。
见[应用开发指南](apps/README.zh-CN.md)。完整构建、致谢与许可证信息见
[英文文档](README.md)。

另见 [开发、隔离测试目录与原生验收](docs/local-development.zh-CN.md)。

[GitHub CI 与发布](docs/github-ci.md)
