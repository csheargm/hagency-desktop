//! Optional local owner policy, outside the shared child's writable namespace.
//! Without a policy file one shared turn may run for an hour in total and has
//! no idle limit. A policy file is taken as written: a field that is absent or
//! null removes that limit. Neither limit is a quota cap.
use super::*;
use serde::Deserialize;
use std::{io::Read, path::Path};
use tokio::time::Instant as Clock;

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct TurnPolicy {
    #[serde(default)]
    no_progress_ms: Option<u64>,
    #[serde(default)]
    max_turn_ms: Option<u64>,
}
pub(super) fn load(path: &Path) -> Result<TurnPolicy, NativeError> {
    let file = match hagency_store::private::open(path, false) {
        Ok(file) => file,
        Err(hagency_store::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TurnPolicy::unwritten());
        }
        Err(_) => return Err(failure(409, "shared_claude_turn_policy_invalid")),
    };
    let mut bytes = Vec::new();
    file.take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() > 4096 {
        return Err(failure(409, "shared_claude_turn_policy_invalid"));
    }
    let policy: TurnPolicy = serde_json::from_slice(&bytes)
        .map_err(|_| failure(409, "shared_claude_turn_policy_invalid"))?;
    for duration in [policy.no_progress_ms, policy.max_turn_ms]
        .into_iter()
        .flatten()
    {
        if duration == 0
            || Clock::now()
                .checked_add(Duration::from_millis(duration))
                .is_none()
        {
            return Err(failure(409, "shared_claude_turn_policy_invalid"));
        }
    }
    Ok(policy)
}
/// How long one shared turn may run when the owner wrote no policy. Without
/// it a room member can hold a pooled Agent until the owner notices and stops
/// the run.
const UNWRITTEN_MAX_TURN_MS: u64 = 60 * 60 * 1000;
impl TurnPolicy {
    /// The policy in force when no policy file exists.
    fn unwritten() -> Self {
        Self {
            no_progress_ms: None,
            max_turn_ms: Some(UNWRITTEN_MAX_TURN_MS),
        }
    }
    pub async fn expired(&self, mut progress: watch::Receiver<Clock>) -> NativeError {
        let maximum = self
            .max_turn_ms
            .map(|ms| Clock::now() + Duration::from_millis(ms));
        loop {
            let idle = self
                .no_progress_ms
                .map(|ms| *progress.borrow() + Duration::from_millis(ms));
            let deadline = maximum.into_iter().chain(idle).min();
            if deadline.is_some_and(|at| at <= Clock::now()) {
                return failure(
                    409,
                    if maximum.is_some_and(|at| Some(at) == deadline) {
                        "shared_claude_owner_duration_reached"
                    } else {
                        "shared_claude_owner_idle_reached"
                    },
                );
            }
            tokio::select! { biased;
                changed = progress.changed() => {
                    if changed.is_err() { return std::future::pending().await; }
                },
                _ = async { match deadline { Some(at) => tokio::time::sleep_until(at).await, None => std::future::pending().await } } => {
                    return failure(409, if maximum.is_some_and(|at| Some(at) == deadline) {
                        "shared_claude_owner_duration_reached"
                    } else { "shared_claude_owner_idle_reached" });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_policy_limits_a_turn_to_one_hour_and_explicit_policy_is_strict() {
        let directory =
            hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = directory.path().join("shared-claude-turn-policy.json");
        let absent = load(&path).unwrap();
        assert!(absent.no_progress_ms.is_none());
        assert_eq!(absent.max_turn_ms, Some(3_600_000));
        // A written policy is taken as written, including "no limit at all".
        hagency_store::private::replace(&path, b"{}").unwrap();
        let unlimited = load(&path).unwrap();
        assert!(unlimited.no_progress_ms.is_none() && unlimited.max_turn_ms.is_none());
        for value in [
            r#"{"noProgressMs":0}"#,
            r#"{"maxTurnMs":-1}"#,
            r#"{"quotaTokens":500}"#,
        ] {
            hagency_store::private::replace(&path, value.as_bytes()).unwrap();
            assert!(load(&path).is_err());
        }
        hagency_store::private::replace(&path, br#"{"noProgressMs":600000,"maxTurnMs":null}"#)
            .unwrap();
        let policy = load(&path).unwrap();
        assert_eq!(policy.no_progress_ms, Some(600000));
        assert!(policy.max_turn_ms.is_none());
    }
    #[tokio::test(start_paused = true)]
    async fn owner_idle_policy_resets_only_on_native_progress() {
        let policy = TurnPolicy {
            no_progress_ms: Some(1000),
            max_turn_ms: None,
        };
        let (send, receive) = watch::channel(Clock::now());
        let waiting = policy.expired(receive);
        tokio::pin!(waiting);
        for _ in 0..5 {
            tokio::select! {
                _ = &mut waiting => panic!("progress was ignored"),
                _ = tokio::time::sleep(Duration::from_millis(900)) => {}
            }
            send.send_replace(Clock::now());
        }
        assert_eq!(waiting.await.code, "shared_claude_owner_idle_reached");
    }
    #[tokio::test(start_paused = true)]
    async fn only_explicit_owner_duration_limits_a_progressing_turn() {
        let policy = TurnPolicy {
            no_progress_ms: None,
            max_turn_ms: Some(1000),
        };
        let (send, receive) = watch::channel(Clock::now());
        let waiting = policy.expired(receive);
        tokio::pin!(waiting);
        loop {
            tokio::select! {
                error = &mut waiting => { assert_eq!(error.code, "shared_claude_owner_duration_reached"); break; },
                _ = tokio::time::sleep(Duration::from_millis(100)) => { send.send_replace(Clock::now()); }
            }
        }
        let (_send, receive) = watch::channel(Clock::now());
        assert!(
            tokio::time::timeout(
                Duration::from_secs(86_400),
                TurnPolicy::default().expired(receive)
            )
            .await
            .is_err()
        );
    }
    #[tokio::test(start_paused = true)]
    async fn a_turn_without_a_written_policy_ends_after_one_hour_even_while_it_progresses() {
        let (send, receive) = watch::channel(Clock::now());
        let began = Clock::now();
        let policy = TurnPolicy::unwritten();
        let waiting = policy.expired(receive);
        tokio::pin!(waiting);
        let error = loop {
            tokio::select! {
                error = &mut waiting => break error,
                _ = tokio::time::sleep(Duration::from_secs(60)) => { send.send_replace(Clock::now()); }
            }
        };
        assert_eq!(error.code, "shared_claude_owner_duration_reached");
        let ran = began.elapsed();
        assert!(
            ran >= Duration::from_secs(3600) && ran < Duration::from_secs(3661),
            "{ran:?}"
        );
    }
}
