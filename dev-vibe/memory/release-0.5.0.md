# Desktop 0.5.0 发布

更新：2026-11-14。

## 2026-11-12 rc.7 发布

修复 rc.6 的 macOS 安装包；由 Claude 执行。

- [v0.5.0-rc.7](https://github.com/hagency-org/hagency-desktop/releases/tag/v0.5.0-rc.7) 于 2026-11-12 18:55:06 UTC 公开为 prerelease。标签和两个安装包源码统一为 Desktop `8df3b2d83f8747e6959e5a3f3092fc8f03183720`（PR #85 合入 main）。Makepad 与 rc.6 相同，为 `200eb92bdf511281ebe37bbecd732f296379af4e`。
- 修复：`build.rs` 在 macOS 的 app／dmg 打包时把包目录设为 `../Resources`，包内图片和字体从 `Contents/Resources` 读取；`src/app.rs` 在启动日志中写明资源来自包内还是源码目录；安装包冒烟检查要求包内标志图片已绘制。
- [release.yml run 43252198296](https://github.com/hagency-org/hagency-desktop/actions/runs/43252198296) 使用 publish=true、macos-arm64-windows，回归门禁、两平台安装包检查和发布全部通过。
- 恰好两个安装包：`hagency-desktop-macos-arm64.dmg`（167,611,780 字节）和 `hagency-desktop-windows-amd64.exe`（121,263,488 字节）。公开附件已重新下载，`SHA256SUMS.txt` 校验通过。
- 公开的 macOS 安装包在构建机以外的 Mac 上用空 profile、隐藏窗口经 `tools/release-desktop-smoke.py` 运行：通过，包目录为 `../Resources`，包内标志已绘制，没有资源错误，rc.6 的缺陷已消除。
- rc.6 页面没有加说明，也没有撤下其 macOS 安装包（由用户决定）。未部署服务端。

## 2026-11-12 rc.6 发布（macOS 安装包有缺陷，由 rc.7 取代）

用户要求提交 JIT 引擎线的改动、让其他人可以运行 JIT 版本，并发布新的 RC；由 Claude 执行。

- [v0.5.0-rc.6](https://github.com/hagency-org/hagency-desktop/releases/tag/v0.5.0-rc.6) 于 2026-11-12 15:22:00 UTC 公开为 prerelease。标签和两个安装包源码统一为 Desktop `36187b5fab526b7fa6dfa54d1692f05aa4eb798c`（PR #81 合入 main 后加 #82、#83、#84 三个发布修复）。stable latest 仍为 v1.1.2。
- Client 已在树内（`crates/hagency-client`），build report 的 clientCommit 与 sourceCommit 相同。Makepad 首次使用 JIT 引擎线：`200eb92bdf511281ebe37bbecd732f296379af4e`（OctoSense fork 分支 `hagency-desktop-0.5.0-rc.6`，记录在 `.jit/sources.lock`）。
- **发布后发现的缺陷：macOS 安装包找不到包内的图片和字体。** 在构建机以外的 Mac 上，登录页标志、图标等图片不显示，文字改用系统字体。原因：`robius-packaging-commands` 0.4.1 为 macOS 传 `MAKEPAD_PACKAGE_DIR=.`，并靠旧引擎的 `MAKEPAD=apple_bundle` 开关定位 `Contents/Resources`；JIT 线引擎不读这个开关，只按可执行文件旁的包目录查找。Windows 安装包传的同样是 `.`，资源就在可执行文件旁，不受影响。CI 冒烟检查只看登录页文字和非空画面，而字体在构建机上能从源码路径读到，所以没有发现；把公开的 DMG 下载到另一台 Mac 上运行才发现。修复：`build.rs` 在 macOS 的 app／dmg 打包时把包目录设为 `../Resources`；冒烟检查改为要求包内标志图片已绘制、启动日志声明包目录、且没有"字体未随包"错误。
- [release.yml run 43224070801](https://github.com/hagency-org/hagency-desktop/actions/runs/43224070801) 使用 publish=true、macos-arm64-windows，是第四次 dispatch。前三次没有发布任何内容：(1) run 43189892280：发布流程没有设置 `HAGENCY_NATIVE_CREDENTIAL_IO=deny`，另有两个只在 Windows 编译的断言已过期（PR #82）；(2) run 43195636756："Verify Windows graphics startup and recreation" 运行的引擎测试在 JIT 线不存在，该步骤被删除，**设备重建不再在本流程中检查**（PR #83）；(3) run 43198816202：安装包冒烟脚本仍用旧引擎的 HTTP 远程端口，一个门禁测试依赖机器上装有 Codex CLI，build report 的 makepadCommit 取自旧的 Cargo.toml 固定值（PR #84）。JIT 线在 GitHub 机器上的安装包任务 macOS 约 92 分钟、Windows 约 85 分钟，加上排队和门禁，每次尝试约 2 小时 15 分。
- 210 项预算／聊天回归全部通过。macOS Desktop 单测 995 通过、2 忽略；Windows 973 通过、2 忽略；两平台各 10 个 app-contract 测试通过。
- 恰好两个安装包：`hagency-desktop-macos-arm64.dmg`（167,713,717 字节）和 `hagency-desktop-windows-amd64.exe`（121,389,765 字节）。两个 build report、三个 SHA256SUMS 及两个安装包共 7 个公开附件已重新下载，大小和 SHA-256 均与 GitHub 元数据、平台报告及校验文件匹配；报告中的 Desktop、Client、Makepad 提交一致。匿名访问 Release 和安装包下载地址均为 HTTP 200。
- 两个安装包在 CI 上用隔离 profile 启动，连续三帧登录页非空；随包运行时为 `octos 2.0.3-rc.13 (fe08d8e 2026-11-12)`。Windows 为可见窗口，渲染截图与原生桌面截图已查看，标志正常；macOS 为隐藏窗口，截图中没有标志（即上述缺陷，当时没有被检查拦下）。没有人工 Windows 11 验收。
- Mac 仍为 ad-hoc 签名、未 notarize；Windows 未签名。升级说明见发布说明：rc.5 及更早版本的 Hosting 设置在首次启动时被丢弃；已保存的登录单向迁入系统凭据库。
- 证据：上述 run 的 artifacts（`release-regressions`、`installer-checks-*`、`candidate-installers-*`，保留 14 天）。未部署服务端，未修改服务器配置。

## 2026-11-07 rc.4 发布

用户要求发布包含最新 Agent 问题卡修复的新 RC；由 Codex 执行。

- [v0.5.0-rc.4](https://github.com/hagency-org/hagency-desktop/releases/tag/v0.5.0-rc.4) 于 2026-11-07 17:35:00 UTC 公开为 prerelease。标签和两个安装包源码统一为 Desktop `5db1ece7f73b1847d0848736f6777c20efca76e9`，包含 rc.3 发布分支及已复核的问题卡提交 `b0243afdd5a07bee9dab14267836ac9d1bcffcec`。stable latest 仍为 v1.1.2。
- Client 固定为 `6cd99ceb84ae407b2315dcd0f5e900022eebac06`；Makepad 保持 `a0ad1839c61cecbd7e83251cfb7d50d7a725ee48`。本次加入消息下方的问题卡，拒绝通过问题通道批准 MCP 工具或技能安装、拒绝其他 Agent 的回答；同一轮问题共用十分钟等待预算，包含加密设备打开及重试；重启或取消后恢复卡片终态，桌面端有界重试暂时无法解密的卡片，并隔离旧登录会话的回调。还包含长 Codex 运行及已完成回答的保留修复。
- [release.yml run 42387632599](https://github.com/hagency-org/hagency-desktop/actions/runs/42387632599) 使用 publish=true、macos-arm64-windows，完整执行成功。110 项预算／聊天回归全部通过，累计 812 次 Rust 测试执行（选择器有重叠，不是独立测试总数）。六组问题卡检查实际执行 54 个测试；四组 rc.3 恢复检查实际执行 21 个测试，均通过。合并回归脚本时保留了两边全部检查，没有绕过门禁或接受零测试结果。
- macOS Desktop 单测 792 通过、2 忽略；Windows 776 通过、2 忽略。两平台各 10 个 app-contract 测试通过，Windows 软件图形设备启动／重建回归实际执行 1 个测试并通过。客户端源码在发布前另通过默认 workspace 3,216 项和 shipping features 1,240 项完整测试，保留既有 ignored／filtered 配置；独立源码复核无剩余阻塞项。
- 恰好两个安装包：`hagency-desktop-macos-arm64.dmg`（162,910,613 字节）和 `hagency-desktop-windows-amd64.exe`（117,586,176 字节）。两个 build report、三个 SHA256SUMS 及两个安装包共 7 个公开附件已重新下载，大小和 SHA-256 均与 GitHub 元数据、平台报告及校验文件匹配；报告中的 Desktop、Client、Makepad 提交一致。匿名访问 Release 和两个安装包下载地址均为 HTTP 200。
- 两个实际安装包均在隔离 profile 下启动并通过连续三帧登录页非空检查，字体／资源错误为零。随包运行时为 `octos 2.0.3-rc.13 (fe08d8e 2026-11-07)`。macOS 渲染截图、Windows 渲染及原生桌面截图共三张已查看；Windows 原生窗口可见。没有执行独立 verify_only 流程，也没有宣称完成了人工 Windows 11 全流程验收或 River 真实加密房间验收。
- Mac 仍为 ad-hoc 签名、未 notarize；Windows 未签名。macOS 资源托管集成在 Desktop 的 Agent → Hosting；没有单独 Hosting 安装包、Windows 后台托管或 Intel macOS 包。发布说明保留已知用量可能低估服务商消耗、兼容服务端要求及既有依赖审计开放项。
- 发布证据位于 `~/.codex/task-reports/hagency-rc4-20261107/release.json`，包含完整工作流、110 项回归报告、两平台测试日志、安装检查、截图及公开附件核验。未部署服务端、修改 River 配置或发送真实房间消息。

## 2026-11-06 rc.3 发布

用户要求发布新的 RC；由 Codex 执行。

- [v0.5.0-rc.3](https://github.com/hagency-org/hagency-desktop/releases/tag/v0.5.0-rc.3) 于 2026-11-06 18:09:33 UTC 公开为 prerelease。标签与全部二进制源码统一为 Desktop `f394023b1fda46c7cfa2684771d6c8a5073eb0f7`，基于最新修复 `91cd4b252df0b0544b2407f9b0db826290247d28`。stable latest 仍为 v1.1.2。
- Client 固定为 `686d473e2ff21da784adfe117f3b17b7efd21302`；Makepad 保持 `a0ad1839c61cecbd7e83251cfb7d50d7a725ee48`。包含按已知用量恢复额度、从未启动消息的有界重试、凭证检查重试与更精确的诊断、单一 owner 目录和集成 Hosting。发布说明明确：缺失用量按零处理可能低估服务商用量并允许超额；消息重试需要服务端支持 `shared-demand-retry-v1`，不能把凭证刷新断言为所有中断的根因。
- [release.yml run 42250311709](https://github.com/hagency-org/hagency-desktop/actions/runs/42250311709) 使用 publish=true、macos-arm64-windows，完整执行成功。104 项预算／聊天回归全部通过，累计 758 次 Rust 测试执行（有重叠，不是独立测试总数）；本次新增 4 组恢复检查，实际执行 21 个测试，全部通过。没有绕过门禁或使用零测试过滤结果。
- macOS Desktop 单测 779 通过、2 忽略；Windows 763 通过、2 忽略。两平台各 10 个 app-contract 测试通过，Windows 软件图形设备启动／重建回归实际执行 1 个测试并通过。源码指针、翻译、图标和 release-assets 检查通过。
- 恰好两个安装包：`hagency-desktop-macos-arm64.dmg`（162,533,214 字节）和 `hagency-desktop-windows-amd64.exe`（117,295,611 字节）。macOS 资源托管已并入 Desktop 的 Agent → Hosting；没有单独 Hosting 安装包，也没有 Windows 后台资源托管或 Intel macOS 包。
- 两个实际安装包均用隔离 profile 启动，连续三帧登录页非空，字体／资源错误为零；随包运行时为 `octos 2.0.3-rc.13 (fe08d8e 2026-11-06)`。macOS 渲染截图、Windows 渲染及原生桌面截图共三张已人工查看，Windows 原生窗口可见。没有运行独立 verify_only 流程，也没有宣称做过人工 Windows 11 全流程验收。
- 两个 build report、三个 SHA256SUMS 加两个安装包，共 7 个公开附件已重新下载，大小和 SHA-256 均与 GitHub 元数据、平台报告及校验文件匹配；报告中的 Desktop、Client、Makepad 提交一致。匿名访问 Release 和两个安装包下载地址均为 HTTP 200。Mac 仍为 ad-hoc 签名、未 notarize；Windows 未签名。
- 发布证据在 `hagency-desktop-rc3-20261106/.run/rc3-release/release.json`，包含完整工作流、回归报告、安装检查、截图和公开附件。未部署服务端或修改 River 可信代理配置，未进行真实账号故障注入；这些不能由安装包回归结果代替。既有依赖审计开放项保留在发布说明中。

## 2026-11-05 rc.2 发布

用户要求重新发布，包含最新的 Windows 工作和聊天历史的本地修复，并要求快速完成；由 Claude 执行。

- [v0.5.0-rc.2](https://github.com/hagency-org/hagency-desktop/releases/tag/v0.5.0-rc.2) 于 2026-11-05 06:16:32 UTC 公开为 prerelease，没有替换 stable latest。标签和全部二进制源码统一为 Desktop `b00459166744dab2ec8433da8176b1521f064e21`（`work/hagency-desktop-local-20261028` 顶端）。
- Client 固定为 `19539818e927a2a2328eebba91d8736fab22c9c6`：Codex 的 agent-startup-history 分支（加密房间分页读取历史、原始／继任／预算三种执行驱动接入、消息和密钥事件唤醒加五秒兜底），含 Claude 的历史硬化修复 94850cad。Codex 通过 PR #4（763b5a7e）合入的内容与 Claude 的直接合并逐字节一致。
- Makepad 固定为 `a0ad1839c61cecbd7e83251cfb7d50d7a725ee48`（OctoSense fork 分支 `fix/hagency-windows-platform-tests`）：即 Codex 的 Windows 系统浏览器分支 `codex/windows-system-browser-20261104` 顶端 7c6f6cc8 加一行测试修复——`platform/src/retained_instances.rs` 的 `sum()` 在 Windows 上因 WebView2 把 serde_json 带入依赖图而类型歧义，makepad-platform 的 lib 测试无法编译，release.yml 的 Windows 图形启动检查因此失败。Codex 若继续该 fork 分支，请基于 a0ad1839。
- 桌面侧还包含待批 Agent 权限请求的应用内提醒、房间角标、Agent 标签提示和原位置卡片（859adaca），以及 Windows 系统浏览器、分隔条命中范围和光标可见性（66515d20）。`windows-single-exe-*` 分支是测试启动器，未纳入。
- 发布由 release.yml 第四次 dispatch（run 41966327510，publish=true，macos-arm64-windows，hosting=true）自动打标签并发布。前三次失败：(1) 待批提醒三条文案未进翻译目录；(2) 门禁自 1827126e 起改在 macOS 15 机器运行，`apple-chat-fonts`／`apple-system-fonts` 失败——探针（临时 probe 分支，run 41964220918，已删除）证实 macOS 15.8.2 镜像没有 PingFang 字体资产，CoreText 把 PingFangSC-Regular 指向 PingFangUI.ttc，其中只有 `.PingFangUIText…-Default` 变量字体和 Medium 字重；现在 `article_makepad::apple_fonts::installed()` 为假时相关断言测试显式跳过并打印原因，目录错误用例改为不依赖系统字体；(3) 上述 Makepad 编译错误。
- 恰好三个安装包：`hagency-desktop-macos-arm64.dmg`（154,623,798 字节）、`hagency-desktop-windows-amd64.exe`（116,828,093）、`hagency-hosting-macos-arm64.dmg`（91,986,406），附两个 build report 和三个 SHA256SUMS。回归门禁在 macOS 15 机器上全部通过；两平台打包任务内的安装启动冒烟、Windows 图形启动检查、Hosting 生命周期和语言切换检查全部通过。rc.1 额外做过的独立 verify_only 验证和人工截图检查本次没有做；Mac 仍为 ad-hoc 签名，Windows 未签名。
- 本地证据在 `hagency-desktop-context-20261104/.rc2-*.log`（按 CI 原命令复现：桌面 766 单测、契约 10、字体 7、makepad-platform 3）。Codex 历史修复的 River 真机复测仍未做，仍待用户授权。macOS 打包任务在 GitHub 2 核机器上耗时约 2 小时；用户问能否自建，River 主机（M2 Pro 10 核，macOS 14.8.5，未装 Rust）可注册为 self-hosted runner 承担 macOS 任务，Linux 机器做不了 Windows 和 macOS 包。

## 2026-11-04 RC 发布

用户明确要求更新 Makepad 并发布 macOS / Windows RC，另提供带中英文切换的 resource daemon 控制应用，并排除 Intel Mac。本次发布完成，以下状态取代 10 月 16 日的暂停发布决定；原记录保留在下方。

- [v0.5.0-rc.1](https://github.com/hagency-org/hagency-desktop/releases/tag/v0.5.0-rc.1) 于 2026-11-04 16:56:52 UTC 公开为 prerelease，没有替换 stable latest。Release ID `433792626`。标签和全部二进制源码统一为 Desktop `1827126ede1bec7b83547b871b735625b6c83890`。
- Client 固定为 `a06e4cc204989bb36e666d59f6510ae7a0489a77`；Makepad 固定为 `ebec2e5c866eae3c791e2f24d59db543cad97a05`，包含 #132 的 Windows 焦点／次级窗口重绘修复，以及软件图形设备回退、静止窗口接收异步 shader 编译结果的修复。Makepad 源码在 OctoSense fork 的 `fix/hagency-windows-software-device` 分支公开。
- 恰好三个安装包：`hagency-desktop-macos-arm64.dmg`、`hagency-desktop-windows-amd64.exe`、`hagency-hosting-macos-arm64.dmg`。Hosting 包含 controller 和 daemon，支持跟随系统／English／简体中文，即时切换并跨重启保存。没有 Intel macOS 包，也不宣称支持 Windows 原生资源托管。
- Mac 安装包在隔离 Apple Silicon worktree、macOS 26.6.2 上构建，使用 pinned cargo-packager 0.10.1。Mac CI 未分配到运行器即因容量不足被取消，因此没有宣称整轮跨平台 CI 成功。Windows [job 126660061999](https://github.com/hagency-org/hagency-desktop/actions/runs/41854657931/job/126660061999) 的应用测试、图形设备启动／重建、打包、安装和真实渲染检查全部 success；所属 run 41854657931 总状态为 failure，不能把它写成整轮 green。
- Mac 760 个 Desktop 单测、10 个 app-contract 单测通过；完整回归 95/95 组、699 次 Rust 测试执行通过（与单测存在重叠，不是合并后的独立测试总数）。翻译检查 2804 条、1596 调用点、172 权限，缺失 0。隔离测试首次因脚本 VM 的 target 路径不匹配而停止，保留失败证据后修正构建路径并从首个未通过检查续跑，没有改动产品源码或跳过检查。
- 实际两个 Desktop 安装包均以干净 profile 启动，连续三帧登录页非空且资源加载正常；Windows 原生桌面截图与渲染截图一致。实际 Hosting DMG 中的 controller／daemon 哈希与构建记录一致，7 项生命周期检查、8 项语言切换／重启保存检查通过。六张实际截图已人工检查。
- Release 附有两个 build report、三个 verification report，以及每平台和总 SHA256SUMS；11 个公开附件的哈希和大小逐一匹配。匿名访问 Release 成功，标签确认为上述源码提交，三个安装包下载地址均返回 HTTP 200。
- Mac 为 ad-hoc 签名、未 notarize；Windows 未签名。发布说明已明确这些限制，保留既有依赖审计的开放事项，没有声称所有上游依赖告警已修复。

## 2026-10-16 历史记录

更新：2026-10-16。

- 用户确认发布目标为 hagency-desktop，要求版本 0.5.0，取代继承的 1.1.2；Cloud 不修改版本。
- hagency 依赖改为 https://github.com/chrislearn/hagency-client.git 的 main 分支；Cargo.lock 锁定 39e990c8d483d6bd29b4135742f486e7b83ad137，14 个引擎 crate 均来自 Git。删除 sibling checkout CI action 和全部调用，构建仍用 --locked。
- Windows cargo check --locked --lib 通过；i18n 缺失 0、主题检查通过。五平台测试及七安装包由 GitHub release.yml 的 publish=true 手动运行验收，全部成功后自动打标签和发布，不能把 dispatch 当成发布成功。
- 发布说明在 packaging/release-notes.md，说明功能、版本回退及 Agent 加密 Room、签名和移动端限制。发布显式 --latest，保证低于 1.1.2 的 0.5.0 仍成为下载入口。
- Cloud 新增 docs-tenant/{en,zh}/agents.md 和 downloads.md，包含七个官方 latest/download 安装包链接；其发布进度在 Cloud dev-vibe/memory/desktop-downloads-and-agents.md 记录。

首轮安装包运行38832134391的macOS ARM64编译和459单测通过，但打包app失败：cargo-packager0.10.1 No matching IconType。此前品牌改动将普通1024 PNG加入icons但未列现成ICNS；上游create_icns_file对1024@1x转换报错，显式ICNS会优先原样复制。现把resources/icon.icns列入Cargo packager icons（Linux仅取PNG、Windows优先ICO，不受影响），新增tools/check-release-icons.py及CI／发布前置校验。Windows locked库编译与真实图标输入检查通过。必须用修复提交重新完整构建后发布，不能重用旧提交混合附件。

最新状态（覆盖此前发布计划）：因发布审计未达标，38832134391、38837925791均取消，没有v0.5.0标签或公开Release。Desktop主lock的Rustls0.23.46、webpki0.103.15、AWS-LC1.18.2/sys0.46.0、Quinn0.11.18兼容安全补丁将漏洞匹配24降到5，仍21其他警告；Windows locked check与456单测通过（2忽略）。随包Octos fe08独立Cargo.lock仍36漏洞匹配+14其他警告，Desktop补丁不会传递过去。实际PDF抽取及RSS属性解析存在需修补路径；不能把所有完整锁文件记录都当成已证实可利用。报告与机器证据见../release-0.5.0/assessment.zh.md。已询问用户是否扩大范围修复上游后再发，未获答复前不再启动正式发布；Cloud上线同样待其独立确认。

最终验证：7ad1b8dca67222bd8312952799b3dc397e12062a的主CI38842267586与五平台构建38842267688均success（远端Mac461pass/2ignore、本地Win456pass/2ignore），翻译缺失0、主题与图标前置检查通过。正式发布仍暂缓；未获新的上游升级范围答复，不自动开启第三轮发布或扩改上游。独立Octos锁已与fe08的Git对象逐字节比对，SHA256 cfed26d0a98f515574607f6015a9547caccace48df43f510c2df38063fe45e64。
