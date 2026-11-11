# Desktop Agent 无回复修复与实测（2026-11-05）

## 问题与最终行为

Agent 身份已为 active，但私聊创建记录停在 `creating / owner_direct_creation_outcome_unknown / room null`。原房间及已送达的 hello 存在，稳定别名目录却返回 HTTP404。旧恢复流程只查询别名，且正确地禁止重复未知结果的 createRoom，因此一直无法完成原房间绑定。同时，服务 watcher 已发现没有连接房间，My Agents 仍显示 Starting。

Pinned Palpo 的 createRoom 先写房间、canonical_alias 和 initial_state，再处理邀请，最后写别名目录；这能产生上述部分创建状态。历史证据不能确定原请求在哪一步失败或取消。

修复通过有界、完整、只读的 joined-room 扫描找回唯一原房间，严格核对创建者、owner/Agent 标记、canonical_alias、私密规则与两方成员；任何歧义或状态读取失败都保留未知结果，不重建 Agent 或房间。随后继续既有身份校验、Matrix 索引和服务器绑定流程。完成发现但没有连接房间的服务显示 Needs attention，正常启动阶段仍显示 Starting。

## PR 与源码

- Client 引擎：[PR #52](https://github.com/hagency-org/hagency-client/pull/52)，目标 `work/hagency-client-local-20261028`，修复提交 `dec4911b45acb6789be96e5a5d75bf8d357f30d1`，基线 `19539818e927a2a2328eebba91d8736fab22c9c6`。
- Desktop：分支 `codex/agent-private-chat-recovery`，PR 目标 `work/hagency-desktop-local-20261028`，基线 `ea4963d13cf7a8fb212c5d2b387714cefc2ba165`。本次同步更新 Desktop、测试依赖和 owner-dashboard 共4处引擎 pin，以及3个 Cargo.lock，全部固定到上述已推送的 Client 提交，不依赖本地 path override。
- Server 实测使用 `work/hagency-server-local-20261028` / `7acbb25c9acb2ddf181b479ecef874662fb583d9`。Server 没有产品代码修改，无需 Server PR；未部署到远端。
- 用户安装版内嵌源码 `b00459166744dab2ec8433da8176b1521f064e21` 与 Desktop 调试基线仅发布文档不同。安装包没有覆盖或发版。

## 回归与真实使用

- Engine：`cargo test --locked -p hagency --lib console::owned_agents::direct::`，7通过，包括候选身份/隐私验证、歧义、状态读取失败、越界/重复列表及原有持久化与索引测试。
- Desktop（macOS arm64）：全量 lib 测试（`onboarding-test-support`），767通过、0失败、2项原有忽略；该轮使用与提交内容相同的明确本地引擎副本。配对锁文件下的 locked check 退出0。
- 本地 Server：127.0.0.1:19888，三个全新 PostgreSQL 数据库、独立 Desktop 配置和测试账号。真实 OAuth → 创建 Local-Debug-Reply-1105 → 私聊创建及 Agent 入群 → 连接本机已有 Codex 账号 → 保存 GPT-6.1-Sol / low → 启动 → 真实回复 `LOCAL_AGENT_REPLY_OK`。账本 settled，input4624 / output33 / cached_input3584 / reasoning_output23，reservation10000。回包后停止该测试服务。
- 原账号、原 Agent、原房间（river.ominix.io:19443）：chris-agent2 的原 hello 保留。状态先正确显示 Needs attention；点击原私聊设置重试，记录从 creating/null 经过 created、pending 到 active/lastError null。服务器回读绑定 active，Agent 实际加入原房间。
- 同一原房间发送 `Please reply only ORIGINAL_AGENT_REPLY_OK. Do not use any tools.`，原 Agent 真实回复 `ORIGINAL_AGENT_REPLY_OK`。账本 settled，input7095 / output9 / cached_input6016，reservation10000。未增加原账号预算，未手改运行记录、删除未知执行或创建替代身份/房间。

## 验证边界与复现

初期使用本地 Cargo override 的调试构建曾被严格源码 pin 门禁拒绝，原始失败日志保留。PR 已改为 GitHub 固定修复提交，4处依赖与3个锁文件的源码 pin 检查通过。首轮最终 pin 驱动的全局 CARGO_TARGET_DIR 将脚本 VM 放到了驱动未预期的位置，完整 bundle 检查因找不到可执行文件而失败；已保留原日志，第二轮使用默认 target 布局重新运行，未修改检查器或产品代码来绕过它。最终远端 pin 的完整回归驱动已退出0，101项检查全部通过（Rust 选择器计数有重叠，不作为独立测试总数）。普通 `cargo build --locked --bin rinx` 也已退出0，无需本地 override。修复相关7项 owner-direct 测试还在 shared-budget-execution/resource-adoption 完整功能配置下再次通过。此文不将本地调试视为签名发布、sandbox 或 two-Agent 资格。

原账号回归曾误启动两个相同数据目录开发实例，导致 OAuth/清理互斥错误；关闭重复实例并确认只运行一个初始化完成的应用后，正常 SDK 刷新恢复，随后完成原房间回包。该调试错误不作为原线上故障的根因。

检出本 Desktop PR 后可直接执行：

```bash
cargo build --locked --bin rinx
bash packaging/run-macos.sh target/debug/rinx
```

独立源码回归：

```bash
python3 tools/check-hagency-regressions.py --output .run/regressions --client /path/to/hagency-client-at-dec4911b
```

启动开发应用前正常退出同一数据目录实例，等待调试启动器初始化完成再选择现有应用。私有配置、凭据、原账号备份、完整日志和服务器/账本收据均保留在 Git 忽略目录，未上传 PR。
