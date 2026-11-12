//! IO scheduling only. An unbounded wait grants no execution or approval authority.
use std::time::Duration;
use tokio::time::Instant;

#[derive(Clone, Copy, Debug)]
pub struct Deadline(Option<Instant>);

impl Deadline {
    pub const UNBOUNDED: Self = Self(None);

    pub fn after(duration: Duration) -> Self {
        Self(Some(Instant::now() + duration))
    }

    pub fn at(self) -> Option<Instant> {
        self.0
    }

    pub fn limit(self, other: impl Into<Self>) -> Self {
        Self(match (self.0, other.into().0) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        })
    }

    pub fn expired(self) -> bool {
        self.0.is_some_and(|until| Instant::now() >= until)
    }

    pub async fn wait(self) {
        match self.0 {
            Some(until) => tokio::time::sleep_until(until).await,
            None => std::future::pending().await,
        }
    }
}

impl From<Instant> for Deadline {
    fn from(until: Instant) -> Self {
        Self(Some(until))
    }
}
