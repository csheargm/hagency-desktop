//! English and Simplified Chinese strings. Every key has both translations:
//! the `match` in [`text`] is exhaustive, so a missing translation fails to
//! compile rather than falling back silently.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Locale {
    #[default]
    En,
    ZhHans,
}

impl Locale {
    /// Maps a BCP 47 tag or POSIX locale (`zh-CN`, `zh_Hans`, `en_US.UTF-8`).
    /// Any Chinese tag selects Simplified Chinese; everything else English.
    pub fn from_tag(tag: &str) -> Self {
        let lower = tag.trim().to_ascii_lowercase();
        if lower.starts_with("zh") {
            Self::ZhHans
        } else {
            Self::En
        }
    }
}

macro_rules! messages {
    ($($key:ident => $en:literal, $zh:literal;)+) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Msg { $($key),+ }
        impl Msg {
            pub const ALL: &'static [Msg] = &[$(Msg::$key),+];
        }
        pub fn text(locale: Locale, msg: Msg) -> &'static str {
            match (locale, msg) {
                $((Locale::En, Msg::$key) => $en, (Locale::ZhHans, Msg::$key) => $zh,)+
            }
        }
    };
}

messages! {
    Title => "Hosting", "托管";
    Refresh => "Refresh", "刷新";
    StatusWorking => "Working…", "正在处理…";
    StatusUnconfirmed => "Not confirmed", "尚未确认";
    StatusAttention => "Needs attention", "需要处理";
    StatusDone => "Done", "已完成";
    StatusUpdate => "Update", "状态更新";
    StatusConnecting => "Connecting…", "正在连接…";
    StatusInventory => "Check resources", "资源列表需检查";
    StatusOffline => "Service offline", "服务未运行";
    StatusDisconnected => "Disconnected", "连接已断开";
    StatusOtherOwner => "Another owner", "其他所有者";
    Refreshing => "Refreshing…", "正在刷新…";
    TabOverview => "Overview", "概览";
    TabResources => "Resources", "资源";
    TabUsage => "Usage", "用量";
    TabActivity => "Activity", "活动";
    HideFilters => "Hide filters", "收起筛选";
    UsageFilters => "Filters & dates", "筛选与日期";
    CustomDates => "Custom dates", "自选日期";
    StartDate => "Start date · UTC", "开始日期 · UTC";
    EndDate => "End date · UTC", "结束日期 · UTC";
    ApplyDates => "Apply dates", "应用日期";
    DateRangeError => "Use YYYY-MM-DD. The end must follow the start, within 366 days.", "请使用 YYYY-MM-DD 格式，结束日期不得早于开始日期，范围最多 366 天。";
    FilterResource => "Resource", "资源";
    CodingAccount => "Coding account", "编码账号";
    Filtered => "Filtered", "已筛选";
    InventoryUnavailable => "Resource list unavailable", "资源列表暂不可用";
    InventoryRetry => "Refresh to try loading your resources again.", "请刷新以重新加载资源。";
    InventoryStale => "Refresh failed. Previously loaded resources are shown below.", "刷新失败，下方显示上次加载的资源。";
    InventoryPartial => "Resource list is incomplete. Usage totals may include resources not listed here.", "资源列表尚不完整。用量总计可能包含未列出的资源。";
    FilterModel => "Model", "模型";
    FilterRequester => "Requester", "请求者";
    AllResources => "All resources", "所有资源";
    AllModels => "All models", "所有模型";
    AllRequesters => "All requesters", "所有请求者";
    ClearFilters => "Clear filters", "清除筛选";

    ConnectingTitle => "Connecting to hosting service…", "正在连接托管服务…";
    ConnectingBody => "Your Agents keep running in the background while this window opens.", "窗口打开期间，你的 Agent 会继续在后台运行。";
    DaemonAbsentTitle => "Hosting service isn't running", "托管服务未运行";
    UnsupportedPlatformTitle => "Background hosting is available on macOS", "后台托管目前支持 macOS";
    UnsupportedPlatformBody => "On this platform Hagency hosts your resource pool while it is running. You can still chat with Agents hosted on another device.", "在此平台上，Hagency 运行期间会托管你的资源池。你仍可与其他设备托管的 Agent 聊天。";
    DaemonAbsentBody => "The background service that hosts your resource pool isn't running on this Mac. Start it to keep your rented Agents working.", "这台 Mac 上托管资源池的后台服务未运行。启动后，出租的 Agent 才能继续工作。";
    IncompatibleTitle => "Update required", "需要更新";
    AuthExpiredTitle => "Sign-in needed", "需要重新登录";
    AuthExpiredBody => "The hosting service's owner sign-in has expired. Sign in again to keep accepting work.", "托管服务的所有者登录已过期。请重新登录以继续接收任务。";
    OwnerMismatchTitle => "Hosting belongs to another account", "托管服务属于其他账号";
    OwnerMismatchBody => "The hosting service on this Mac serves another account, so its data isn't shown here. One Mac user can host one account's resource pool at a time.", "这台 Mac 上的托管服务属于另一个账号，因此不在此显示其数据。一个 Mac 用户同一时间只能托管一个账号的资源池。";
    FailedTitle => "Can't reach the hosting service", "无法连接托管服务";
    ShowingLastKnown => "Showing the last known status.", "当前显示的是最后一次获取的状态。";
    SetUpHosting => "Set up hosting", "设置托管";
    StartService => "Start hosting service", "启动托管服务";
    SignInAgain => "Sign in again", "重新登录";
    TryAgain => "Try again", "重试";

    StateUnconfiguredTitle => "Hosting isn't set up", "尚未设置托管";
    StateUnconfiguredBody => "Set up owner hosting to serve your rented Agents while Hagency is closed.", "设置所有者托管后，即使关闭 Hagency 也能为出租的 Agent 提供服务。";
    StateStartingTitle => "Starting hosting", "正在启动托管";
    StateStartingBody => "Connecting workers. New work is accepted once hosting is on.", "正在连接工作进程。托管开启后才会接收新任务。";
    StateRunningTitle => "Hosting is on", "托管已开启";
    StateRunningBody => "Accepting new work for your rented Agents.", "正在为出租的 Agent 接收新任务。";
    StatePausedTitle => "Hosting is paused", "托管已暂停";
    StatePausedBody => "No new work is accepted. Resume to accept requests again.", "不再接收新任务。恢复后会重新接收请求。";
    StateDrainingTitle => "Pausing", "正在暂停";
    StateDrainingBody => "No new work is accepted. Running tasks are finishing.", "不再接收新任务，正在运行的任务即将完成。";
    StateStoppedTitle => "Hosting is stopped", "托管已停止";
    StateStoppedBody => "Your resources and settings are saved. Resume hosting when you are ready.", "资源和设置已保留，您可以随时恢复托管。";
    StateOfflineTitle => "Hosting is offline", "托管离线";
    StateOfflineBody => "The Hagency server can't be reached. Work resumes when the connection returns.", "无法连接 Hagency 服务器。连接恢复后会继续工作。";
    StateFailedTitle => "Hosting stopped with an error", "托管因错误停止";
    StateFailedBody => "Review the details, then resume hosting.", "请查看详细信息，然后恢复托管。";

    ActiveRuns => "Running", "运行中";
    PendingApprovals => "Awaiting approval", "待审批";
    PauseHosting => "Pause hosting", "暂停托管";
    ResumeHosting => "Resume hosting", "恢复托管";
    StopHosting => "Stop hosting", "停止托管";
    Stop => "Stop", "停止";
    ReviewApprovals => "Review approvals", "查看审批";
    StopConfirmTitle => "Stop hosting?", "停止托管？";
    StopConfirmBody => "New work stops. Running tasks may be interrupted.", "停止接收新任务，运行中的任务可能被中断。";
    Cancel => "Cancel", "取消";
    CheckStatus => "Check status", "查询状态";
    Retry => "Retry", "重试";
    Dismiss => "Dismiss", "关闭";

    PlansTitle => "Provider plans", "服务商套餐";
    PlansEmpty => "No provider accounts reported yet. Plan limits appear after the hosting service observes an account.", "尚未报告服务商账号。托管服务观测到账号后会显示套餐限额。";
    NotReported => "Not reported", "未报告";
    ResetNotReported => "Reset time not reported", "未报告重置时间";
    NeverObserved => "Not observed yet", "尚未观测";
    AccountUnverifiedNote => "Account identity isn't verified, so it isn't combined with other accounts.", "账号身份未经验证，因此不会与其他账号合并。";
    UnassignedAccount => "Account not verified", "账号未验证";
    UnassignedAccountNote => "These resources aren't linked to a verified provider account. Their usage isn't counted against any plan above.", "这些资源未关联到已验证的服务商账号，其用量不计入上方任何套餐。";
    ResourcesOnAccount => "Resources on this account", "使用此账号的资源";

    AccountFresh => "Up to date", "最新";
    AccountStale => "Out of date", "已过时";
    AccountPartial => "Partial data", "部分数据";
    AccountLimited => "Limit reached", "已达限额";
    AccountUnsupported => "Not reported by provider", "服务商不提供";
    AccountUnknown => "Not observed yet", "尚未观测";
    AccountUnverified => "Identity not verified", "身份未验证";
    StatusUnrecognized => "Unrecognized status", "无法识别的状态";

    UsageTitle => "Measured usage", "实测用量";
    UsageTotal => "Total tokens", "总 Token";
    TokensInput => "Input", "输入";
    TokensOutput => "Output", "输出";
    TokensCacheRead => "Cache read", "缓存读取";
    TokensCacheWrite => "Cache write", "缓存写入";
    CacheHitRatio => "Cache hit ratio", "缓存命中率";
    Unavailable => "Unavailable", "不可用";
    Unknown => "Unknown", "未知";
    UsageComplete => "All measurements complete", "所有测量均完整";
    UsagePartial => "Partial: some measurements are missing, so totals are a lower bound.", "部分数据：缺少部分测量，总量为下限。";
    UsageNoData => "No measured usage reported.", "尚无实测用量。";
    EstimatedCost => "API-equivalent estimate", "按 API 价格估算";
    EstimatedCostNote => "Not a subscription charge.", "并非订阅费用。";
    NotEstimated => "Not estimated", "未估算";
    PeriodDay => "Today", "今天";
    PeriodWeek => "This week", "本周";
    PeriodMonth => "This month", "本月";
    PeriodUnreported => "Period not reported", "未报告统计周期";
    ByResource => "By resource", "按资源";
    ByAccount => "By account", "按账号";
    AllAccounts => "All accounts", "全部账号";
    UsageBoundary => "Calendar periods are for reporting only; providers apply their own limit windows.", "日历周期仅用于统计；服务商按各自的限额窗口计算。";

    ResourcesTitle => "Resources", "资源";
    ResourcesEmpty => "No hosted resources yet. Resources appear after you publish an Agent pool.", "暂无托管资源。发布 Agent 资源池后会显示在这里。";
    Agents => "Agents", "Agent 数量";
    Budget => "Budget", "预算";
    NoBudget => "Budget not reported", "未提供预算信息";
    TokensUsed => "Tokens used", "已用 Token";
    ResourceMeasured => "Measured", "已测量";
    ResourcePartial => "Partially measured", "部分测量";
    ResourceUnmeasured => "Not measured", "未测量";
    ResourceIdle => "Idle", "空闲";
    Details => "Details", "详情";
    Back => "Back", "返回";
    TechnicalDetails => "Technical details", "技术信息";
    ResourceId => "Resource ID", "资源 ID";
    AccountId => "Account ID", "账号 ID";
    Provider => "Provider", "服务商";
    Model => "Model", "模型";
    ResourceGone => "This resource is no longer reported by the hosting service.", "托管服务已不再报告此资源。";

    ActivityTitle => "Activity", "活动";
    ActivityEmpty => "No recent activity.", "暂无近期活动。";
    ActivityRunning => "Running", "运行中";
    ActivityPending => "Pending", "等待中";
    ActivitySucceeded => "Done", "已完成";
    ActivityFailed => "Failed", "失败";
    ActivityUncertain => "Outcome unknown", "结果未知";
    ActivityInfo => "Info", "信息";
    ActivityWarning => "Warning", "警告";

    AttentionTitle => "Needs attention", "需要处理";
    AttentionNone => "Nothing needs your attention.", "暂无需要处理的事项。";
    AttentionPartialUsage => "Some usage measurements are incomplete.", "部分用量测量不完整。";
    AttentionUnassigned => "Some resources aren't linked to a verified account.", "部分资源未关联到已验证的账号。";

    JustNow => "just now", "刚刚";
    Today => "Today", "今天";
    Tomorrow => "Tomorrow", "明天";
    SignedInAs => "Owner", "所有者";
    NoOwner => "No owner signed in", "未登录所有者";

    ReasonRevisionConflict => "Hosting changed. Review and retry.", "托管状态已变化，请查看后重试。";
    ReasonOwnerScope => "This controller isn't authorized for the signed-in owner.", "此控制器未获得当前所有者的授权。";
    ReasonStopped => "The hosting service is stopped.", "托管服务已停止。";
    ReasonBusy => "The hosting service is busy. Try again shortly.", "托管服务繁忙，请稍后重试。";
    ReasonNotSupported => "This hosting service doesn't support that action.", "此托管服务不支持该操作。";
    ReasonShuttingDown => "The hosting service is shutting down.", "托管服务正在关闭。";
    ReasonVersion => "The hosting service uses a different version. Update Hagency.", "托管服务版本不一致，请更新 Hagency。";
    ReasonUnknown => "The hosting service rejected the request.", "托管服务拒绝了该请求。";
    ReasonNotConnected => "Not connected to the hosting service.", "未连接到托管服务。";

    SetupTitle => "Set up background hosting", "设置后台托管";
    SetupBody => "Hagency hosts your resource pool with a background service on this Mac, so your rented Agents keep working while Hagency is closed. Setup opens your browser once to sign this Mac in.", "Hagency 通过这台 Mac 上的后台服务托管你的资源池，关闭 Hagency 后出租的 Agent 仍能继续工作。设置时会打开一次浏览器，为这台 Mac 登录。";
    SetupReplaceTitle => "Set up hosting again", "重新设置托管";
    SetupReplaceBody => "An earlier hosting sign-in on this Mac uses a different resource folder. Setting up again signs it out and hosts this account's resources.", "这台 Mac 上较早的托管登录使用了另一个资源目录。重新设置会退出该登录，并托管此账号的资源。";
    SetupOtherOwnerTitle => "Hosting serves another account", "托管服务属于其他账号";
    SetupOtherOwnerBody => "The hosting service on this Mac hosts another account's resource pool. One Mac user can host one account's resource pool at a time. To host this account here, sign the other account out of Hagency on this Mac first.", "这台 Mac 上的托管服务正在托管另一个账号的资源池。一个 Mac 用户同一时间只能托管一个账号的资源池。如需在这里托管当前账号，请先在这台 Mac 的 Hagency 中退出另一个账号。";
    SetupPreparing => "Preparing hosting setup…", "正在准备托管设置…";
    SetupSigningIn => "Complete sign-in in your browser…", "请在浏览器中完成登录…";
    SetupRegistering => "Signed in. Registering this Mac’s executor…", "已登录，正在登记这台 Mac 的执行端…";
    SetupCancelling => "Cancelling sign-in safely…", "正在安全取消登录…";
    SetupEnabling => "Starting the background service…", "正在启动后台服务…";
    SetupConfirming => "Waiting for hosting to start…", "正在等待托管启动…";
    KeepRunningTitle => "Keep the resource pool running after Desktop quits", "关闭桌面端后继续运行资源池";
    KeepRunningOn => "Rented Agents keep working while Hagency is closed.", "关闭 Hagency 后，出租的 Agent 仍会继续工作。";
    KeepRunningOff => "Hagency pauses hosting when it quits and resumes it the next time it starts.", "Hagency 退出时会暂停托管，下次启动时恢复。";
    UninstallService => "Uninstall hosting service", "卸载托管服务";
    UninstallConfirmTitle => "Uninstall the hosting service?", "卸载托管服务？";
    UninstallConfirmBody => "Hosting stops until you start the service again. Your setup, sign-in and resources are kept.", "在重新启动服务前，托管会停止。你的设置、登录和资源都会保留。";
    Uninstall => "Uninstall", "卸载";
    ForeignService => "The hosting service runs from another app, such as an earlier Hagency Hosting. Use this app's service so hosting updates with Hagency.", "托管服务由另一个应用（例如早期的 Hagency Hosting）运行。请改用本应用的服务，使托管随 Hagency 一起更新。";
    UseThisService => "Use this app's service", "使用本应用的服务";
    HostingConfirmed => "Background hosting is on for this Mac.", "这台 Mac 已开启后台托管。";
    ServiceStarting => "Starting the hosting service…", "正在启动托管服务…";
    ServiceRemoving => "Removing the hosting service…", "正在移除托管服务…";
    ServiceRemoved => "The hosting service was removed. Your setup is kept; start the service again here.", "托管服务已移除。设置已保留，可在此重新启动服务。";
    HostingBusy => "Another hosting step is still running. Wait for it to finish, then retry.", "另一个托管步骤仍在进行。请等待完成后重试。";
}

/// Static codes a hosting helper may report, matched as whole words in its
/// error output. Everything else is `setup_failed`.
pub const SETUP_FAILURE_CODES: &[&str] = &[
    "daemon_other_owner_enrolled", "matrix_account_mismatch", "daemon_logout_pending",
    "daemon_sign_in_cancelled", "daemon_enrollment_cancelled", "daemon_enrollment_detached",
    "daemon_already_running", "unsupported_hagency_server", "matrix_authorization_unavailable",
    "matrix_authorization_required", "owner_root_busy_or_invalid", "owner_root_not_registered",
    "owner_root_invalid", "owner_root_record_invalid", "owner_root_registry_unavailable",
    "owner_identity_invalid", "daemon_owner_root_invalid", "daemon_enrollment_storage_unavailable",
    "daemon_session_storage_unavailable", "daemon_socket_unsafe", "native_resume_unavailable",
    "daemon_install_location_unsupported", "daemon_service_stop_pending",
];

pub fn tr(locale: Locale, msg: Msg) -> &'static str {
    text(locale, msg)
}

/// Replaces `{name}` placeholders. Unknown placeholders are left visible so a
/// translation mistake is noticed rather than silently dropping data.
pub fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = template.to_owned();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

/// Parameterized phrases. Each returns a complete sentence in the locale.
pub mod phrase {
    use super::{Locale, fill};

    pub fn updated(locale: Locale, age: &str) -> String {
        match locale {
            Locale::En => fill("Updated {age}", &[("age", age)]),
            Locale::ZhHans => fill("更新于{age}", &[("age", age)]),
        }
    }
    pub fn observed(locale: Locale, age: &str) -> String {
        match locale {
            Locale::En => fill("Observed {age}", &[("age", age)]),
            Locale::ZhHans => fill("观测于{age}", &[("age", age)]),
        }
    }
    pub fn used_percent(locale: Locale, percent: &str) -> String {
        match locale {
            Locale::En => fill("{p} used", &[("p", percent)]),
            Locale::ZhHans => fill("已用 {p}", &[("p", percent)]),
        }
    }
    pub fn resets(locale: Locale, remaining: &str, at: &str) -> String {
        match locale {
            Locale::En => fill("Resets in {r} · {at}", &[("r", remaining), ("at", at)]),
            Locale::ZhHans => fill("{r}后重置 · {at}", &[("r", remaining), ("at", at)]),
        }
    }
    pub fn reset_due(locale: Locale, at: &str) -> String {
        match locale {
            Locale::En => fill("Reset due {at} · awaiting new observation", &[("at", at)]),
            Locale::ZhHans => fill("应于 {at} 重置 · 等待新的观测", &[("at", at)]),
        }
    }
    pub fn incompatible(locale: Locale, ours: u32, theirs: Option<u32>) -> String {
        let theirs = theirs.map(|v| v.to_string());
        match (locale, theirs) {
            (Locale::En, Some(v)) => format!("This dashboard uses API version {ours}; the hosting service reports version {v}. Update Hagency so both match."),
            (Locale::En, None) => format!("This dashboard uses API version {ours}; the hosting service didn't report a compatible version. Update Hagency so both match."),
            (Locale::ZhHans, Some(v)) => format!("此面板使用 API 版本 {ours}，托管服务报告的版本为 {v}。请更新 Hagency 使两者一致。"),
            (Locale::ZhHans, None) => format!("此面板使用 API 版本 {ours}，托管服务未报告兼容版本。请更新 Hagency 使两者一致。"),
        }
    }
    pub fn running_and_pending(locale: Locale, running: u64, pending: u64) -> String {
        match locale {
            Locale::En => format!("{running} running · {pending} awaiting approval"),
            Locale::ZhHans => format!("{running} 个运行中 · {pending} 个待审批"),
        }
    }
    pub fn draining(locale: Locale, running: u64) -> String {
        match locale {
            Locale::En if running == 1 => "1 running task is finishing; no new work is accepted.".into(),
            Locale::En => format!("{running} running tasks are finishing; no new work is accepted."),
            Locale::ZhHans => format!("{running} 个正在运行的任务即将完成；不再接收新任务。"),
        }
    }
    pub fn submitting(locale: Locale, action: &str) -> String {
        match locale {
            Locale::En => format!("{action}: sending…"),
            Locale::ZhHans => format!("{action}：正在发送…"),
        }
    }
    pub fn awaiting(locale: Locale, action: &str) -> String {
        match locale {
            Locale::En => format!("{action}: accepted, waiting for the hosting service to confirm…"),
            Locale::ZhHans => format!("{action}：已接受，等待托管服务确认…"),
        }
    }
    pub fn awaiting_drain(locale: Locale, action: &str, running: u64) -> String {
        match locale {
            Locale::En => format!("{action}: accepted. Waiting for {running} running task(s) to finish."),
            Locale::ZhHans => format!("{action}：已接受，等待 {running} 个运行中的任务完成。"),
        }
    }
    pub fn confirmed(locale: Locale, action: &str, at: &str) -> String {
        match locale {
            Locale::En => format!("{action}: confirmed {at}."),
            Locale::ZhHans => format!("{action}：已于 {at} 确认。"),
        }
    }
    pub fn unchanged(locale: Locale, action: &str) -> String {
        match locale {
            Locale::En => format!("{action}: no change was needed."),
            Locale::ZhHans => format!("{action}：无需更改。"),
        }
    }
    pub fn rejected(locale: Locale, action: &str, reason: &str, code: &str) -> String {
        match locale {
            Locale::En => format!("{action} failed: {reason} ({code})"),
            Locale::ZhHans => format!("{action}失败：{reason}（{code}）"),
        }
    }
    pub fn uncertain(locale: Locale, action: &str) -> String {
        match locale {
            Locale::En => format!("{action}: outcome unknown. Check status without sending again."),
            Locale::ZhHans => format!("{action}：结果尚未确认。查询状态不会重复发送。"),
        }
    }
    pub fn checking(locale: Locale, action: &str) -> String {
        match locale {
            Locale::En => format!("{action}: checking the outcome…"),
            Locale::ZhHans => format!("{action}：正在查询结果…"),
        }
    }
    pub fn open_failed(locale: Locale, action: &str, reason: &str) -> String {
        match locale {
            Locale::En => format!("{action} failed: {reason}"),
            Locale::ZhHans => format!("{action}失败：{reason}"),
        }
    }
    pub fn approvals_waiting(locale: Locale, n: u64) -> String {
        match locale {
            Locale::En if n == 1 => "1 request is awaiting your approval.".into(),
            Locale::En => format!("{n} requests are awaiting your approval."),
            Locale::ZhHans => format!("{n} 个请求正在等待你的审批。"),
        }
    }
    pub fn account_limited(locale: Locale, label: &str) -> String {
        match locale {
            Locale::En => format!("{label} reached a provider limit."),
            Locale::ZhHans => format!("{label} 已达到服务商限额。"),
        }
    }
    pub fn account_near_limit(locale: Locale, label: &str, percent: &str) -> String {
        match locale {
            Locale::En => format!("{label} has used {percent} of a plan window."),
            Locale::ZhHans => format!("{label} 的某个套餐窗口已用 {percent}。"),
        }
    }
    pub fn failed_activities(locale: Locale, n: usize) -> String {
        match locale {
            Locale::En if n == 1 => "1 activity failed or has an unknown outcome.".into(),
            Locale::En => format!("{n} activities failed or have an unknown outcome."),
            Locale::ZhHans => format!("{n} 项活动失败或结果未知。"),
        }
    }
    pub fn resource_line(locale: Locale, provider: &str, model: &str, agents: Option<u64>) -> String {
        match (locale, agents) {
            (Locale::En, Some(1)) => format!("{provider} · {model} · 1 Agent"),
            (Locale::En, Some(n)) => format!("{provider} · {model} · {n} Agents"),
            (Locale::ZhHans, Some(n)) => format!("{provider} · {model} · {n} 个 Agent"),
            (Locale::En, None) => format!("{provider} · {model} · Agent count unavailable"),
            (Locale::ZhHans, None) => format!("{provider} · {model} · Agent 数量暂不可用"),
        }
    }
    pub fn budget_of(locale: Locale, _used: &str, budget: &str) -> String {
        match locale {
            Locale::En => format!("Owner limit: {budget} tokens"),
            Locale::ZhHans => format!("主人设置的上限：{budget} Token"),
        }
    }
    /// A hosting setup failure, from a static code only. Raw helper output
    /// can contain account data and is never shown.
    pub fn setup_failure(locale: Locale, code: &str) -> &'static str {
        let zh = locale == Locale::ZhHans;
        match (code, zh) {
            ("daemon_other_owner_enrolled" | "matrix_account_mismatch", false) => "This Mac's hosting service belongs to another account. Sign in with the account shown here, or sign the other account out of Hagency on this Mac first.",
            ("daemon_other_owner_enrolled" | "matrix_account_mismatch", true) => "这台 Mac 的托管服务属于另一个账号。请使用这里显示的账号登录，或先在这台 Mac 的 Hagency 中退出另一个账号。",
            ("daemon_logout_pending", false) => "The previous sign-out is still finishing. Keep Hagency connected until it completes, then retry.",
            ("daemon_logout_pending", true) => "上次退出尚未完成。请保持 Hagency 连接直到完成，然后重试。",
            ("setup_timed_out", false) => "Setup timed out. Check the server connection, then retry.",
            ("setup_timed_out", true) => "设置超时。请检查服务器连接后重试。",
            ("daemon_sign_in_cancelled" | "daemon_enrollment_cancelled" | "daemon_enrollment_detached", false) => "Sign-in was cancelled or expired. Retry when you are ready to finish it in your browser.",
            ("daemon_sign_in_cancelled" | "daemon_enrollment_cancelled" | "daemon_enrollment_detached", true) => "登录已取消或过期。准备好后请重试，并在浏览器中完成登录。",
            ("daemon_already_running", false) => "Another hosting setup or service is using this Mac's hosting folder. Wait for it to finish, then retry.",
            ("daemon_already_running", true) => "另一个托管设置或服务正在使用这台 Mac 的托管目录。请等待其完成后重试。",
            ("unsupported_hagency_server", false) => "This server does not support hosting setup. Check the server and its Hagency version.",
            ("unsupported_hagency_server", true) => "此服务器不支持托管设置。请检查服务器及其 Hagency 版本。",
            ("matrix_authorization_unavailable" | "matrix_authorization_required", false) => "Browser authorization is unavailable. Check your connection, finish the browser sign-in, then retry.",
            ("matrix_authorization_unavailable" | "matrix_authorization_required", true) => "浏览器授权暂不可用。请检查连接并完成浏览器登录，然后重试。",
            ("owner_root_busy" | "owner_root_busy_or_invalid", false) => "Signed in, but this account's resources are still in use by another Hagency on this Mac. Quit it, then start the hosting service.",
            ("owner_root_busy" | "owner_root_busy_or_invalid", true) => "已登录，但这台 Mac 上另一个 Hagency 仍在使用此账号的资源。请退出它，然后启动托管服务。",
            ("registration_pending", false) => "Signed in, but this Mac's executor registration did not finish. Start the hosting service to retry it.",
            ("registration_pending", true) => "已登录，但这台 Mac 的执行端注册尚未完成。请启动托管服务以重试。",
            ("owner_root_not_registered" | "owner_root_invalid" | "owner_root_record_invalid" | "owner_root_registry_unavailable"
                | "owner_identity_invalid" | "daemon_owner_root_invalid" | "daemon_enrollment_storage_unavailable"
                | "daemon_session_storage_unavailable" | "daemon_socket_unsafe" | "native_resume_unavailable", false) => "This Mac's hosting data could not be opened safely. Check the Hagency data folders and their permissions, then retry.",
            ("owner_root_not_registered" | "owner_root_invalid" | "owner_root_record_invalid" | "owner_root_registry_unavailable"
                | "owner_identity_invalid" | "daemon_owner_root_invalid" | "daemon_enrollment_storage_unavailable"
                | "daemon_session_storage_unavailable" | "daemon_socket_unsafe" | "native_resume_unavailable", true) => "无法安全打开这台 Mac 的托管数据。请检查 Hagency 数据目录及其权限后重试。",
            ("daemon_install_location_unsupported", false) => "Move Hagency Desktop to the Applications folder, then start the hosting service again.",
            ("daemon_install_location_unsupported", true) => "请将 Hagency Desktop 移到“应用程序”文件夹，然后重新启动托管服务。",
            ("daemon_service_stop_pending", false) => "The previous hosting service is still stopping. Wait for it to finish, then retry.",
            ("daemon_service_stop_pending", true) => "之前的托管服务仍在停止。请等待完成后重试。",
            ("service_start_failed", false) => "The hosting service did not start. Check Login Items in System Settings, then retry.",
            ("service_start_failed", true) => "托管服务未能启动。请检查“系统设置”中的登录项，然后重试。",
            ("hosting_not_confirmed", false) => "The service is installed, but hosting has not reported running yet. Refresh to check its status, or retry.",
            ("hosting_not_confirmed", true) => "服务已安装，但托管尚未报告运行。请刷新查看状态，或重试。",
            ("service_remove_failed", false) => "The hosting service could not be removed. Retry, or check Login Items in System Settings.",
            ("service_remove_failed", true) => "无法移除托管服务。请重试，或检查“系统设置”中的登录项。",
            ("helper_unavailable", false) => "Hagency could not start its hosting helper. Reinstall Hagency Desktop, then retry.",
            ("helper_unavailable", true) => "Hagency 无法启动托管助手。请重新安装 Hagency Desktop 后重试。",
            ("sign_in_required", false) => "Sign in to Hagency to set up hosting for this account.",
            ("sign_in_required", true) => "请先登录 Hagency，再为此账号设置托管。",
            (_, false) => "Hosting setup failed. Check the server, your account and the app installation, then retry.",
            (_, true) => "托管设置失败。请检查服务器、账号和应用安装后重试。",
        }
    }
    pub fn approx_usd(_locale: Locale, value: &str) -> String {
        format!("≈ {value}")
    }
    pub fn window_label(locale: Locale, label: &str, length: Option<&str>) -> String {
        match (locale, length) {
            (_, None) => label.to_owned(),
            (Locale::En, Some(l)) => format!("{label} · {l}"),
            (Locale::ZhHans, Some(l)) => format!("{label} · {l}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_is_translated_and_distinct_per_locale() {
        for &msg in Msg::ALL {
            let en = text(Locale::En, msg);
            let zh = text(Locale::ZhHans, msg);
            assert!(!en.trim().is_empty(), "{msg:?} has empty English");
            assert!(!zh.trim().is_empty(), "{msg:?} has empty Chinese");
            assert!(
                zh.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) || zh == en,
                "{msg:?} Chinese text has no CJK characters: {zh}"
            );
        }
    }

    #[test]
    fn locale_tags_map_to_supported_locales() {
        assert_eq!(Locale::from_tag("zh-CN"), Locale::ZhHans);
        assert_eq!(Locale::from_tag("zh_Hans_CN.UTF-8"), Locale::ZhHans);
        assert_eq!(Locale::from_tag("ZH-hant"), Locale::ZhHans);
        assert_eq!(Locale::from_tag("en_US.UTF-8"), Locale::En);
        assert_eq!(Locale::from_tag(""), Locale::En);
    }

    #[test]
    fn every_helper_code_has_its_own_setup_message_and_unknown_text_is_never_shown() {
        for locale in [Locale::En, Locale::ZhHans] {
            let generic = phrase::setup_failure(locale, "setup_failed");
            for code in SETUP_FAILURE_CODES {
                assert_ne!(phrase::setup_failure(locale, code), generic, "{code} in {locale:?}");
            }
            let raw = "token=private /private/account/path";
            assert_eq!(phrase::setup_failure(locale, raw), generic);
            assert!(!generic.contains("private"));
        }
    }

    #[test]
    fn fill_keeps_unknown_placeholders_visible() {
        assert_eq!(fill("{a} and {b}", &[("a", "1")]), "1 and {b}");
    }
}
