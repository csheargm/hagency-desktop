//! Usage caps for a server's own Ollama: per-user rate, daily tokens, one run
//! at a time and a run deadline; a server-wide concurrency limit with a fair
//! (round-robin by user) queue and a daily token budget; admin exemption and
//! a kill switch. Daily token totals persist in one file and survive restart.
//! All limits live in [`CapsConfig`], the one place they are configured.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;

pub const SECONDS_PER_DAY: i64 = 86_400;
const SECONDS_PER_MINUTE: i64 = 60;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapsConfig {
    pub requests_per_minute: u32,
    pub user_tokens_per_day: u64,
    pub server_tokens_per_day: u64,
    /// Concurrent runs across the server (1-2 on a CPU host).
    pub server_concurrency: usize,
    pub max_run_seconds: u64,
    /// Admin-exempt Matrix user IDs (no per-user caps; server caps still apply).
    pub exempt_users: BTreeSet<String>,
}
impl Default for CapsConfig {
    fn default() -> Self {
        Self {
            requests_per_minute: 6,
            user_tokens_per_day: 200_000,
            server_tokens_per_day: 2_000_000,
            server_concurrency: 1,
            max_run_seconds: 600,
            exempt_users: BTreeSet::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    Stopped,
    RateLimited { retry_at: i64 },
    UserDailyLimit { resets_at: i64 },
    ServerDailyLimit { resets_at: i64 },
    AlreadyRunning,
}
impl Refusal {
    /// The user-facing message. Times are UTC `HH:MM`.
    pub fn message(&self) -> String {
        match self {
            Self::Stopped => "AI runs are paused by the server admin.".into(),
            Self::RateLimited { retry_at } => format!("Too many requests — try again at {}.", clock(*retry_at)),
            Self::UserDailyLimit { resets_at } => format!("Daily limit reached — resets at {} UTC.", clock(*resets_at)),
            Self::ServerDailyLimit { resets_at } => {
                format!("The server's daily AI budget is used up — resets at {} UTC.", clock(*resets_at))
            }
            Self::AlreadyRunning => "You already have a run in progress — wait for it to finish.".into(),
        }
    }
}
fn clock(unix: i64) -> String {
    let s = unix.rem_euclid(SECONDS_PER_DAY);
    format!("{:02}:{:02}", s / 3600, (s % 3600) / 60)
}
fn day(now: i64) -> i64 {
    now.div_euclid(SECONDS_PER_DAY)
}

#[derive(Default, Serialize, Deserialize)]
struct Persisted {
    day: i64,
    users: BTreeMap<String, u64>,
    server: u64,
}

/// Outcome of [`Caps::request`]: run now, or wait in the fair queue.
#[derive(Debug, PartialEq, Eq)]
pub enum Admission {
    Run,
    Queued { position: usize },
}

pub struct Caps {
    config: CapsConfig,
    path: PathBuf,
    tokens: Persisted,
    stopped: bool,
    recent: BTreeMap<String, VecDeque<i64>>,
    running: BTreeSet<String>,
    /// One waiting entry per user; served round-robin.
    queue: VecDeque<String>,
}
impl Caps {
    pub fn open(config: CapsConfig, path: PathBuf) -> std::io::Result<Self> {
        if config.requests_per_minute == 0 || config.server_concurrency == 0 || config.max_run_seconds == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "caps must be at least 1"));
        }
        let tokens = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Persisted::default(),
            Err(e) => return Err(e),
        };
        Ok(Self { config, path, tokens, stopped: false, recent: BTreeMap::new(),
            running: BTreeSet::new(), queue: VecDeque::new() })
    }
    pub fn config(&self) -> &CapsConfig {
        &self.config
    }
    /// Kill switch: refuse new runs. Callers also interrupt running ones.
    pub fn set_stopped(&mut self, stopped: bool) {
        self.stopped = stopped;
        if stopped {
            self.queue.clear();
        }
    }
    fn roll(&mut self, now: i64) {
        if self.tokens.day != day(now) {
            self.tokens = Persisted { day: day(now), ..Default::default() };
        }
    }
    /// Admits or queues one run for `user`, or says which cap refuses it.
    pub fn request(&mut self, user: &str, now: i64) -> Result<Admission, Refusal> {
        self.roll(now);
        let resets_at = (day(now) + 1) * SECONDS_PER_DAY;
        if self.stopped {
            return Err(Refusal::Stopped);
        }
        if self.tokens.server >= self.config.server_tokens_per_day {
            return Err(Refusal::ServerDailyLimit { resets_at });
        }
        // One run per user for everyone, admins included: `running` is a set.
        if self.running.contains(user) || self.queue.iter().any(|u| u == user) {
            return Err(Refusal::AlreadyRunning);
        }
        if !self.config.exempt_users.contains(user) {
            if self.tokens.users.get(user).copied().unwrap_or(0) >= self.config.user_tokens_per_day {
                return Err(Refusal::UserDailyLimit { resets_at });
            }
            let recent = self.recent.entry(user.into()).or_default();
            recent.retain(|t| now - t < SECONDS_PER_MINUTE);
            if recent.len() >= self.config.requests_per_minute as usize {
                return Err(Refusal::RateLimited { retry_at: recent[0] + SECONDS_PER_MINUTE });
            }
            recent.push_back(now);
        }
        if self.running.len() < self.config.server_concurrency && self.queue.is_empty() {
            self.running.insert(user.into());
            return Ok(Admission::Run);
        }
        self.queue.push_back(user.into());
        Ok(Admission::Queued { position: self.queue.len() })
    }
    /// Ends `user`'s run, charges its tokens durably and returns the next
    /// queued user to start (oldest waiting user first: one entry per user).
    pub fn finish(&mut self, user: &str, tokens: u64, now: i64) -> std::io::Result<Option<String>> {
        self.roll(now);
        self.running.remove(user);
        *self.tokens.users.entry(user.into()).or_default() += tokens;
        self.tokens.server += tokens;
        self.save()?;
        if self.tokens.server >= self.config.server_tokens_per_day {
            // Waiting users are refused on their next request with the reset time.
            self.queue.clear();
        }
        if self.stopped || self.running.len() >= self.config.server_concurrency {
            return Ok(None);
        }
        let next = self.queue.pop_front();
        if let Some(next) = &next {
            self.running.insert(next.clone());
        }
        Ok(next)
    }
    pub fn user_tokens_today(&self, user: &str) -> u64 {
        self.tokens.users.get(user).copied().unwrap_or(0)
    }
    fn save(&self) -> std::io::Result<()> {
        let temporary = self.path.with_extension("tmp");
        std::fs::write(&temporary, serde_json::to_vec(&self.tokens)?)?;
        std::fs::rename(temporary, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOON: i64 = 20_000 * SECONDS_PER_DAY + 12 * 3600;
    fn caps(config: CapsConfig) -> (tempfile::TempDir, Caps) {
        let dir = tempfile::tempdir().unwrap();
        let caps = Caps::open(config, dir.path().join("caps.json")).unwrap();
        (dir, caps)
    }
    fn wide() -> CapsConfig {
        CapsConfig { server_concurrency: 8, ..Default::default() }
    }
    #[test]
    fn requests_per_minute_are_limited_per_user() {
        let (_d, mut c) = caps(CapsConfig { requests_per_minute: 2, ..wide() });
        for t in [0, 1] {
            assert_eq!(c.request("@a:x", NOON + t), Ok(Admission::Run));
            c.finish("@a:x", 0, NOON + t).unwrap();
        }
        let refused = c.request("@a:x", NOON + 2).unwrap_err();
        assert_eq!(refused, Refusal::RateLimited { retry_at: NOON + 60 });
        assert_eq!(refused.message(), "Too many requests — try again at 12:01.");
        assert_eq!(c.request("@b:x", NOON + 2), Ok(Admission::Run));
        assert_eq!(c.request("@a:x", NOON + 61), Ok(Admission::Run));
    }
    #[test]
    fn one_concurrent_run_per_user() {
        let (_d, mut c) = caps(wide());
        assert_eq!(c.request("@a:x", NOON), Ok(Admission::Run));
        assert_eq!(c.request("@a:x", NOON), Err(Refusal::AlreadyRunning));
    }
    #[test]
    fn user_daily_tokens_persist_across_restart_and_reset_next_day() {
        let (dir, mut c) = caps(CapsConfig { user_tokens_per_day: 100, ..wide() });
        c.request("@a:x", NOON).unwrap();
        c.finish("@a:x", 150, NOON).unwrap();
        let mut c = Caps::open(CapsConfig { user_tokens_per_day: 100, ..wide() }, dir.path().join("caps.json")).unwrap();
        let refused = c.request("@a:x", NOON + 5).unwrap_err();
        assert_eq!(refused.message(), "Daily limit reached — resets at 00:00 UTC.");
        assert_eq!(c.request("@a:x", NOON + SECONDS_PER_DAY), Ok(Admission::Run));
    }
    #[test]
    fn server_daily_budget_stops_everyone() {
        let (_d, mut c) = caps(CapsConfig { server_tokens_per_day: 10, ..wide() });
        c.request("@a:x", NOON).unwrap();
        c.finish("@a:x", 10, NOON).unwrap();
        assert!(matches!(c.request("@b:x", NOON), Err(Refusal::ServerDailyLimit { .. })));
    }
    #[test]
    fn queued_runs_do_not_start_once_the_server_budget_is_spent() {
        let (_d, mut c) = caps(CapsConfig { server_tokens_per_day: 10, server_concurrency: 1, ..Default::default() });
        c.request("@a:x", NOON).unwrap();
        c.request("@b:x", NOON).unwrap();
        assert_eq!(c.finish("@a:x", 10, NOON).unwrap(), None);
        assert!(matches!(c.request("@b:x", NOON + 1), Err(Refusal::ServerDailyLimit { .. })));
    }
    #[test]
    fn zero_caps_are_rejected_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let zero = CapsConfig { requests_per_minute: 0, ..Default::default() };
        assert!(Caps::open(zero, dir.path().join("c.json")).is_err());
    }
    #[test]
    fn server_queue_is_fair_across_users() {
        let (_d, mut c) = caps(CapsConfig { server_concurrency: 1, ..Default::default() });
        assert_eq!(c.request("@a:x", NOON), Ok(Admission::Run));
        assert_eq!(c.request("@b:x", NOON), Ok(Admission::Queued { position: 1 }));
        assert_eq!(c.request("@c:x", NOON), Ok(Admission::Queued { position: 2 }));
        // @a cannot jump the queue with a second request.
        assert_eq!(c.request("@a:x", NOON), Err(Refusal::AlreadyRunning));
        assert_eq!(c.finish("@a:x", 1, NOON).unwrap().as_deref(), Some("@b:x"));
        assert_eq!(c.request("@a:x", NOON + 1), Ok(Admission::Queued { position: 2 }));
        assert_eq!(c.finish("@b:x", 1, NOON).unwrap().as_deref(), Some("@c:x"));
        assert_eq!(c.finish("@c:x", 1, NOON).unwrap().as_deref(), Some("@a:x"));
    }
    #[test]
    fn kill_switch_refuses_and_drops_the_queue() {
        let (_d, mut c) = caps(CapsConfig { server_concurrency: 1, ..Default::default() });
        c.request("@a:x", NOON).unwrap();
        c.request("@b:x", NOON).unwrap();
        c.set_stopped(true);
        assert_eq!(c.request("@c:x", NOON), Err(Refusal::Stopped));
        assert_eq!(c.finish("@a:x", 0, NOON).unwrap(), None);
        c.set_stopped(false);
        assert_eq!(c.request("@b:x", NOON + 1), Ok(Admission::Run));
    }
    #[test]
    fn admin_exempt_user_skips_user_caps_but_not_server_budget() {
        let config = CapsConfig { requests_per_minute: 1, user_tokens_per_day: 1, server_tokens_per_day: 100,
            exempt_users: ["@admin:x".to_string()].into(), ..wide() };
        let (_d, mut c) = caps(config);
        assert_eq!(c.request("@admin:x", NOON), Ok(Admission::Run));
        assert_eq!(c.request("@admin:x", NOON), Err(Refusal::AlreadyRunning), "admins hold one slot too");
        c.finish("@admin:x", 0, NOON).unwrap();
        for _ in 0..2 {
            assert_eq!(c.request("@admin:x", NOON), Ok(Admission::Run));
            c.finish("@admin:x", 50, NOON).unwrap();
        }
        assert!(matches!(c.request("@admin:x", NOON), Err(Refusal::ServerDailyLimit { .. })));
    }
}
