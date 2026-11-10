# Desktop 开发与本地验收

[English](local-development.md) · [使用指南](hagency-quickstart.zh-CN.md) · [README](../README.zh-CN.md)

## 源码布局与构建

主人引擎位于 `crates/hagency-client`，保留独立的 Rust 库工作区。Desktop 和
owner-dashboard 通过仓库内路径依赖它，构建与发布检查使用同一份源码。无需同级
Client 检出、远程提交指针或旧网页 UI 构建。详见[库说明](../crates/hagency-client/README.md)。

Desktop 固定 Rust 1.98.0，server 使用自己的工具链。macOS 需要 Xcode command-line
tools 和 CMake（`brew install cmake`）；Linux 需要 CMake、Clang/libclang、OpenSSL、
SQLite、pkg-config 与原生显示/音频库，见[上游构建依赖](robrix-upstream-readme.md)。
使用已提交的 Cargo lockfile，不复用 server 的 target 产物。

```sh
cargo build --locked -p rinx --bin rinx
cargo run --locked --bin rinx
```

包名/二进制仍为 `rinx`，应用名称为 Hagency Desktop，默认 features 已支持原生管理。
macOS Cargo 通过 `packaging/run-macos.sh` 在 `target/debug/.rinx-dev/` 生成临时签名的
开发 bundle。替换 bundle 前先退出上一实例；launcher 转发自己启动的应用日志与停止信号。

调试器连接或 LaunchServices 启动停滞时，可在 checkout 中运行
`RINX_DEV_NO_BUNDLE=1 cargo run --locked --bin rinx`。此时系统权限提示可能归属于
终端。构建完成或取得 PID 不代表应用已初始化、登录或加载 Project。

## 隔离测试账号和数据

```sh
mkdir -p .run
HAGENCY_DESKTOP_DATA_DIR="$PWD/.run/desktop-test-profile" \
cargo run --locked --bin rinx
```

后续使用同一绝对路径；该设置隔离数据与缓存，`RINX_DATA_DIR`、`ROBRIX_DATA_DIR`
是较低优先级别名。同一目录不要同时开多个实例。新目录没有保存账号，也不复制其他
账号的密钥、账本或模型关联。私人状态和日志不提交 Git，日志可能含账号/Room 元数据。

server 按[安装说明](../../hagency-server/README.zh-CN.md)与
[连接排障](../../hagency-server/docs/LOCAL_DEPLOYMENT.zh-CN.md)启动，选择源码或 Compose
一种方式。Desktop 填写服务器 origin，如 `http://127.0.0.1:8088`。HTTP OAuth
仅允许 loopback IP；本地域名需 HTTPS、匹配的 Pasion issuer 与可信证书。
浏览器 discovery 正常但 `auth_metadata` 失败时，检查 server 容器的 issuer 访问。

保存服务器下拉项直接提供 origin；选择“其他服务器…”才显示手动地址输入。
在浏览器完成 Pasion 授权；不带 Hagency/Pasion 的普通 Matrix server 不是当前登录目标。

## 自动检查

```sh
cargo test --locked -p rinx --lib
cargo test --locked -p rinx home::project_tree::tests --lib -- --test-threads=1
cargo test --locked -p rinx browser_login_tests --lib -- --test-threads=1
cargo test --locked --manifest-path crates/article-core/Cargo.toml
python3 tools/wechat-ux/check_i18n.py
```

Project 导航测试覆盖独立文件夹展开、名称打开详情与账号/登录刷新；登录控件测试覆盖
保存/手动地址切换。它们不代替原生视觉/指针验收。ignored live 测试需专用私人 fixture，
不能记为已通过。协议/数据库检查见[server 测试](../../hagency-server/docs/TESTING.zh-CN.md)。

## 原生验收步骤

1. 使用临时 Pasion 账号登录，确认聊天同步、已有 Project 自动显示；使用同一数据
   目录重开验证会话恢复。
2. 点 Project 文件夹展开/收起，点名称打开详情；检查键盘操作、Room 切换及目标窗口
   尺寸下的浅色/深色模式。显示已有 Project 不应要求先创建新 Project。
3. 创建/采纳有权限的 Project 与讨论 Room，邀请测试成员，验证独立 Room 成员权限；
   用另一账号检查主人访问边界。
4. 创建个人 Agent，等待主人私聊就绪；按[Agent 使用指南](hagency-quickstart.zh-CN.md)
   配置 Codex、模型和策略。
5. 若本轮包含推理验收，显式启动一个有权限的未加密范围，发送测试任务并确认真实
   模型回复。结束后停止该范围；重启后已停止范围继续停止，恢复之前显式运行的范围
   仍需新鲜权限与完整本地账本。

区分恢复会话和首次登录、fixture 回复和模型回复、控件自动检查和可见原生操作。
未知执行/费用需保留 hold，删除数据不是恢复手段。

## 续权、租约与回复调试

在 checkout 中将日志保存到私人目录，便于关联发送时间与恢复阶段：

```sh
umask 077
mkdir -p .run/diagnostics
RUST_LOG='warn,hagency=debug,matrix_sdk::authentication::oauth=debug' \
cargo run --locked --bin rinx > .run/diagnostics/desktop.log 2>&1
```

关注 `owner_adoption_grant`（创建 Hagency 会话）、`owner_renewal`
（续权）、`lease_renewal` / `lease_renewal_deadline`（租约）与
`processing_notice`（👀 请求已接收）。日志记录阶段、耗时和固定错误码；
SDK OAuth 刷新成功不代表 Hagency 续权或消息投递已成功。

临时网络/服务错误返回可重试状态，原权限只保持到原截止时间；成功续权后恢复。
`invalid_grant`、`invalid_client` 等登录终止错误进入重新登录；过期租约停止执行。
在线状态若带执行错误，应显示需要处理，不能只以心跳成功判定运行健康。
回复与 👀 必须在实际 Room 中验收；协议 fixture 通过不能替代视觉和真实模型验证。

服务器日志可启用 `hagency_agent_service=debug`，观察身份校验、会话连接等待与续权
耗时。`database_connection_discarded` 表示连接已关闭，或取消/回滚失败留下未结束
事务，连接已丢弃；`database_reconnected` 表示后续操作已建立新连接。同步检查 PostgreSQL 的
`pg_stat_activity`、`pg_blocking_pids`，健康检查成功不证明 Agent 请求没有锁等待。
分享日志前删去 token、密钥和私人消息；SDK 较宽的 INFO 日志可能记录加密备份密钥。

## 打包

助手二进制见[Octos 打包](../packaging/README-octos.md)，macOS 发布使用
`packaging/build-macos-dmg.sh`，iOS 使用 `packaging/build-ios-testflight.sh`。
发布需平台签名配置，Cargo 开发 bundle 不等于发布包。
[Rinx deployment](rinx-deployment.md) 说明 mini-app/Octos 宿主边界，与原生 Hagency
Agent 执行分开。
