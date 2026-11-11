# 设备验证与密钥恢复

在 Settings → Account 的 Encryption and recovery 中查看设备与恢复状态。
设备验证确认设备身份；读取加密历史还需要对应的房间密钥。

## 首次设置

1. 在尚未配置恢复的账号上选择 Set up recovery。
2. 将生成的恢复密钥保存到私有文件，并另存一份到设备之外。
3. 保存成功后，选择 I saved my key — enable recovery。
4. 可用的房间密钥随后进入备份；Back up pending room keys 可等待当前已知密钥上传。

密钥文件使用操作系统的私有文件权限，拒绝覆盖已有文件和读取符号链接。
设置进度只记录公开的账号、设备、密钥标识及校验元数据，不将恢复密钥写入进度文件。
已经开始发表的设置保留原意图；中断后使用 Load saved key to resume 继续原设置。

## 恢复已有账号

输入已有恢复密钥，或通过 Restore from key file 选择密钥文件。
恢复会导入已有身份秘密及备份密钥，再按需下载服务器中可用的房间密钥；恢复失败会显示错误，
可以用原密钥重试。流程保留既有身份、Secret Storage 和备份，不自动重置它们。

状态分别表示设备是否已验证、当前设备是否持有身份密钥，以及是否已连接房间密钥备份。
“账号已配置恢复”不能作为当前设备已恢复密钥的证明。恢复完成后关闭 Settings，
原来打开的聊天会处理积压的解密更新，无需重启或重新选择 Room。

验证弹窗及其异步结果绑定到账号会话、对端用户和 SAS flow。切换账号或重新登录后，
旧结果和旧窗口输入失效；关闭账号任务域时清除内存中保管的恢复密钥。

## SDK 与来源

从 Client PR #38（4fc7c3caa）和 #43（091931f21）迁入，适配 Desktop 当前的样式、
客户端槽修订号、设置导航及 crossbeam 时间线通道。

使用官方 `matrix-org/matrix-rust-sdk` Git 提交
`61905c3e46496340fad7faa6c5f77b8f363d88e7`，不再依赖个人 SDK fork。
该提交位于官方主干，包版本仍为 0.19.1，但已经包含晚于 crates.io 0.19.1 的
`Recovery::enable().with_key(&key)` 接口。预先生成、私有保存并确认的原密钥，
在启用时由官方接口发表；失败后仍可使用原密钥和公开进度继续。

Desktop 与 `crates/hagency-client` 独立工作区及锁文件固定到同一 SDK 来源。
直接使用的 Ruma 家族同步到 SDK 使用的官方提交
`caf1ac05a9fd347158b60505ef8546a2ff18ae7c`，避免同名类型来自不同来源。
事件缓存明确启用；同时适配官方 Git 版的可选缓存、线程、消息反应及过期令牌 API。
OAuth 重新授权测试使用正式的设备 scope，并继续验证原设备 ID、原凭据及取消行为。
Makepad pin 保持原样。SQL 迁移文件已核对，与 registry 0.19.1 一致。

历史恢复采用 `AfterDecryptionFailure`：恢复身份秘密和备份密钥后，按需下载缺失的
房间密钥。SDK 在备份可用时重试内存中的未解密事件，密钥到达后发出解密更新。
界面不将“备份已连接”等同于“全部历史已下载”，也不再依赖定制的 OneShot 补丁。

实现参考 Element X iOS 的两阶段密钥激活流程。已有账号使用 `recover()`，
保留原身份及备份；不采用可能删除并重建异常备份的 `recover_and_fix_backup()`。

官方 SDK 未包含原 fork 的秘密导入锁补丁。Desktop 不启用跨进程 Olm 替换，
账号切换会取消并等待该账号任务结束；这不代表 SDK 的并发替换问题已经修复。
若以后增加共享密钥库或跨进程恢复，需要重新验证该条件。

## 验证边界

回归覆盖私有密钥文件、原密钥设置进度、状态区分、过期结果、精确验证会话，
以及返回被设置页覆盖的原生聊天后刷新缓存。
测试使用隔离资料目录和合成密钥，没有操作用户账号、生产备份或系统凭据。
人工生产登录与跨平台实机恢复仍需单独验收。

### SDK 定向复验

在上述固定提交的未修改 SDK 检出中，使用
`cargo test --locked -p matrix-sdk --features testing --test integration`：

| 筛选器 | 通过数量 |
| --- | ---: |
| `test_secret_store_create_pregenerated_key` | 1 |
| `encryption::recovery::test_recovery_setup` | 2 |
| `encryption::secret_storage::test_restore_cross_signing_from_secret_store` | 1 |
| `encryption::verification::` | 4 |
| `encryption::backups::test_enable_from_secret_storage_and_download_after_utd` | 2 |

这些用例实际执行官方 SDK、HTTP 模拟服务、密钥导入与历史消息解密。
Desktop 完整库、预算/聊天门禁、翻译检查及二进制构建另行执行。
