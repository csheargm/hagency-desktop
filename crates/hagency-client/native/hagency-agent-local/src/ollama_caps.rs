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
    /// Charged when a run starts and settled to actual usage when it ends, so
    /// a crash mid-run keeps the run charged.
    pub reserve_tokens_per_run: u64,
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
            reserve_tokens_per_run: 16_000,
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
    Storage,
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
            Self::Storage => "AI usage could not be recorded, so the run did not start.".into(),
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
    /// Reservations of runs in progress (already included in the totals).
    #[serde(default)]
    reserved: BTreeMap<String, u64>,
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
        let mut tokens: Persisted = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Persisted::default(),
            Err(e) => return Err(e),
        };
        // A reservation left by a crash stays charged; no run is in progress now.
        tokens.reserved.clear();
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
            self.start(user).map_err(|_| Refusal::Storage)?;
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
        let reserved = self.tokens.reserved.remove(user).unwrap_or(0);
        let total = self.tokens.users.entry(user.into()).or_default();
        *total = total.saturating_sub(reserved) + tokens;
        self.tokens.server = self.tokens.server.saturating_sub(reserved) + tokens;
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
            self.start(next)?;
        }
        Ok(next)
    }
    /// Marks `user` running and durably charges the reservation first.
    fn start(&mut self, user: &str) -> std::io::Result<()> {
        let reserve = self.config.reserve_tokens_per_run;
        *self.tokens.users.entry(user.into()).or_default() += reserve;
        self.tokens.server += reserve;
        self.tokens.reserved.insert(user.into(), reserve);
        self.running.insert(user.into());
        self.save()
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
mod tests;
