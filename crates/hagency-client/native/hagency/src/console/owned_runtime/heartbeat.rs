//! The Agent lease heartbeat: renews the execution lease and refreshes the
//! owner's device while a runtime holds the lease.
//!
//! Why (Nov 18, Betty's window, load average ~46 from parallel builds): each
//! lease renewal queued on the owner's renewal line, which was stuck for 17 s,
//! and the heartbeat gave up at the local lease deadline with the request
//! still unanswered; a running issue turn was dropped as "outcome unknown".
//! Only the server knows whether the lease is still live: it renews a lease
//! only while `expires_at_ms > now` and never revives one that ended. So the
//! heartbeat renews on a proof that is still valid (no queue), waits for an
//! answer a short grace past the local deadline, and stops when the server
//! refuses or the grace ends unanswered. A renewed lease is used only after
//! `deadline` accepts it (in the future, at most a lease length away).
use super::*;
use crate::console::Error as LoginError;
use std::sync::atomic::{AtomicU8, Ordering};

/// How long past the local lease deadline a renewal answer is still awaited.
/// The turn keeps running locally meanwhile; every server step it takes is
/// still checked against the server's own lease.
const LEASE_ANSWER_GRACE: Duration = Duration::from_secs(15);
/// Pause between renewal attempts that got no answer.
const RENEWAL_RETRY: Duration = Duration::from_secs(1);
/// The lease length asked for on acquire and renewal; the server caps it at
/// the owner's proof.
pub(super) const LEASE_TTL_MS: i64 = 30_000;

enum Renewal {
    Renewed(Lease),
    Unanswered,
    Stop(&'static str),
}
type RefreshTask = tokio::task::JoinHandle<Result<AuthorizedDevice, LoginError>>;
/// A device refresh that ends the heartbeat (`Stop`) or got no answer (`None`).
type RefreshMiss = Option<&'static str>;

pub(super) fn heartbeat_next(now: Instant, expiry: Instant) -> Instant {
    now + Duration::from_secs(5).min(expiry.saturating_duration_since(now) / 2)
}
pub(super) async fn heartbeat(
    console: Console,
    device: AuthorizedDevice,
    lease: Lease,
    stop: watch::Sender<bool>,
    mut receiver: watch::Receiver<bool>,
    failed: Arc<AtomicU8>,
) {
    let Some(reason) = beat(&console, device, lease, &mut receiver).await else {
        return;
    };
    tracing::warn!(stage = reason, "Agent heartbeat stopped");
    let terminal = matches!(
        reason,
        "lease_renewal_rejected" | "device_identity_changed" | "device_authorization_required"
    );
    failed.store(if terminal { 1 } else { 2 }, Ordering::Release);
    stop.send_replace(true);
}
/// Why the heartbeat stopped, or `None` when it was canceled.
async fn beat(
    console: &Console,
    mut device: AuthorizedDevice,
    mut lease: Lease,
    receiver: &mut watch::Receiver<bool>,
) -> Option<&'static str> {
    let Ok(mut expiry) = deadline(&lease) else {
        return Some("lease_deadline_elapsed");
    };
    let mut next = heartbeat_next(Instant::now(), expiry.min(device.valid_until()));
    // Since when the device refresh after a renewal has had no answer, and
    // that refresh while it runs: one at a time, never a pile on the lock.
    let mut refresh_unanswered: Option<Instant> = None;
    let mut refreshing = None;
    loop {
        let give_up = expiry + LEASE_ANSWER_GRACE;
        tokio::select! { biased; _=canceled(receiver)=>return None, _=tokio::time::sleep_until(next.min(give_up).into())=>{} }
        if Instant::now() >= give_up {
            return Some("lease_renewal_deadline");
        }
        match renew(console, &device, &lease, expiry, receiver).await? {
            Renewal::Renewed(value) => {
                let Ok(fresh) = deadline(&value) else {
                    return Some("renewed_lease_expired");
                };
                (lease, expiry) = (value, fresh);
            }
            Renewal::Unanswered => {
                next = Instant::now() + RENEWAL_RETRY;
                continue;
            }
            Renewal::Stop(reason) => return Some(reason),
        }
        next = heartbeat_next(Instant::now(), expiry);
        match refresh(console, &device, &mut refreshing, next, receiver).await? {
            Ok(fresh) => {
                refresh_unanswered = None;
                device = fresh;
                next = heartbeat_next(Instant::now(), expiry.min(device.valid_until()));
            }
            Err(None) => {
                if let Some(reason) = refresh_missed(&mut refresh_unanswered) {
                    return Some(reason);
                }
            }
            Err(Some(reason)) => return Some(reason),
        }
    }
}
/// One renewal on the current proof. An answer is awaited until the local
/// deadline plus `LEASE_ANSWER_GRACE`; the server, not the local clock,
/// decides whether the lease is still live.
async fn renew(
    console: &Console,
    device: &AuthorizedDevice,
    lease: &Lease,
    expiry: Instant,
    receiver: &mut watch::Receiver<bool>,
) -> Option<Renewal> {
    let pin = DevicePin::from(device);
    let operation = Op::Renew {
        lease: lease.reference(),
        ttl_ms: LEASE_TTL_MS,
    };
    let answer = tokio::select! { biased; _=canceled(receiver)=>return None,
    _=tokio::time::sleep_until((expiry + LEASE_ANSWER_GRACE).into())=>return Some(Renewal::Stop("lease_renewal_deadline")),
    result=console.execution_api_live_pinned(operation,&pin)=>result };
    let late = Instant::now() >= expiry;
    Some(match answer {
        Ok(Response::Lease(value)) => Renewal::Renewed(value),
        Err(error) if !matches!(error.status, 401 | 403 | 409) => {
            tracing::warn!(
                stage = "lease_renewal",
                status = error.status,
                late,
                remaining_ms = expiry.saturating_duration_since(Instant::now()).as_millis() as u64,
                "Lease renewal got no answer; retrying until the server answers"
            );
            Renewal::Unanswered
        }
        // Past the local deadline a refusal is the server ending the lease by
        // expiry, which a restart can recover; before it, a revocation.
        _ if late => Renewal::Stop("lease_ended_on_server"),
        _ => Renewal::Stop("lease_renewal_rejected"),
    })
}
/// Refresh the owner's device (and so the owner's server proof) after a
/// renewal, waiting no later than `until`, the next renewal. The refresh runs
/// as its own task: one that has no answer yet stays in `pending` and is
/// awaited again after the next renewal, never canceled mid-renewal (which
/// could drop rotated owner credentials) nor started twice.
async fn refresh(
    console: &Console,
    device: &AuthorizedDevice,
    pending: &mut Option<RefreshTask>,
    until: Instant,
    receiver: &mut watch::Receiver<bool>,
) -> Option<Result<AuthorizedDevice, RefreshMiss>> {
    let task = pending.get_or_insert_with(|| {
        let console = console.clone();
        tokio::spawn(async move { console.authorized_device().await })
    });
    let fresh = tokio::select! { biased; _=canceled(receiver)=>return None,
    _=tokio::time::sleep_until(until.into())=>return Some(Err(None)),
    joined=task=>joined.unwrap_or(Err(LoginError::Unavailable)) };
    *pending = None;
    Some(match fresh {
        Ok(fresh) if same_device(&fresh, device) => Ok(fresh),
        Ok(_) => Err(Some("device_identity_changed")),
        Err(LoginError::Unavailable) => Err(None),
        Err(_) => Err(Some("device_authorization_required")),
    })
}
/// Note a device refresh with no answer; the reason to stop once given up.
fn refresh_missed(unanswered: &mut Option<Instant>) -> Option<&'static str> {
    let now = Instant::now();
    let since = *unanswered.get_or_insert(now);
    if device_refresh_given_up(since, now) {
        return Some("device_refresh_transport");
    }
    tracing::warn!(
        stage = "device_refresh",
        unanswered_ms = now.saturating_duration_since(since).as_millis() as u64,
        "Device refresh got no answer; retrying with the next renewal"
    );
    None
}
fn same_device(fresh: &AuthorizedDevice, device: &AuthorizedDevice) -> bool {
    fresh.origin() == device.origin()
        && fresh.issuer() == device.issuer()
        && fresh.subject() == device.subject()
        && fresh.user_id() == device.user_id()
        && fresh.owner_mxid() == device.owner_mxid()
        && fresh.device_id() == device.device_id()
        && fresh.generation() == device.generation()
}
/// Whether a device refresh that has had no answer since `since` is given up:
/// after the same grace as an unanswered running check.
pub(super) fn device_refresh_given_up(since: Instant, now: Instant) -> bool {
    now.saturating_duration_since(since) >= RUNNING_CHECK_GRACE
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unanswered_device_refresh_is_retried_for_one_lease_length() {
        let since = Instant::now();
        assert!(!device_refresh_given_up(since, since));
        let almost = since + RUNNING_CHECK_GRACE - Duration::from_millis(1);
        assert!(!device_refresh_given_up(since, almost));
        assert!(device_refresh_given_up(since, since + RUNNING_CHECK_GRACE));
    }
    #[test]
    fn heartbeat_schedule_precedes_short_proof_deadline() {
        let now = Instant::now();
        assert_eq!(heartbeat_next(now, now + Duration::from_secs(30)), now + Duration::from_secs(5));
        assert_eq!(heartbeat_next(now, now + Duration::from_secs(4)), now + Duration::from_secs(2));
        assert_eq!(heartbeat_next(now, now), now);
    }
}
