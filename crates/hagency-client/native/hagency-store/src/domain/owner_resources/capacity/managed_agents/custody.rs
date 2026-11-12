//! Unwired native effect custody. The proof types deliberately have no
//! production constructor until the authenticated shared transport is reviewed.
use super::*;
mod authority;
pub mod budget;
mod calls;
mod matrix_check;
mod observations;
mod provision;
mod reconciliation;
mod runtime;
pub mod shared_execution;
use authority::*;
pub use calls::{ManagedCallClaim, ManagedCallCustody, ManagedCallHistory};
pub use reconciliation::{
    InterruptedCall, InterruptedCallPage, InterruptedCallQuery, InterruptedCallReconciliation,
    ReconcileInterruptedCall,
};
pub use matrix_check::{
    AGENT_MATRIX_CAPABILITY, ManagedAgentMatrixChallenge, ManagedAgentMatrixObservation,
    VerifiedManagedMatrixCheck,
};
pub use observations::{
    ManagedObservationReceipt, ManagedProvisionObservation, ManagedUsageObservation,
};
pub use provision::{ManagedProvisionClaim, ManagedProvisionCustody, ManagedProvisionHistory};
pub use runtime::{
    AGENT_RUNTIME_CAPABILITY, ManagedRuntimeAcknowledgement, ManagedRuntimeClaim,
    ManagedRuntimeCommand, ManagedRuntimeCustody, ManagedRuntimeDelivery,
    ManagedRuntimeEvidenceSink, ManagedRuntimeHistory, ManagedRuntimeProvisionCommand,
    ManagedRuntimeReadiness, ManagedRuntimeReceipt, ManagedRuntimeRecordedProcess,
    ManagedRuntimeRecoverInput, ManagedRuntimeResultInput, ManagedRuntimeStopCommand,
    VerifiedManagedRuntimeDelivery,
};

/// Future trusted shared-transport output; never constructible from script JSON.
/// One assignment generation is pinned permanently for this bounded checkpoint.
pub struct ManagedAssignmentAuthority {
    enrollment: VerifiedManagementEnrollment,
    child: String,
    assignment: String,
    generation: u64,
    expires: Instant,
}
/// A current central execution with an immutable input commitment. No model
/// prompt, provider credential, filesystem path or tool configuration is carried.
pub struct ManagedCallAuthority {
    assignment: ManagedAssignmentAuthority,
    execution: String,
    execution_digest: String,
    estimated_tokens: u64,
}
/// Current same-installation cleanup may collect old evidence, never issue work.
pub struct ManagedCleanupAuthority {
    enrollment: VerifiedManagementEnrollment,
    child: String,
    expires: Instant,
}
impl ManagedAssignmentAuthority {
    fn check(&self, registry: &Inner) -> Result<(), Error> {
        self.enrollment.check()?;
        identifier(&self.child, 128)?;
        identifier(&self.assignment, 128)?;
        exact(self.generation)?;
        if registry.closed.load(Ordering::Acquire) || Instant::now() >= self.expires {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
    fn deadline(&self) -> Instant {
        self.expires.min(self.enrollment.expires)
    }
    #[cfg(test)]
    pub(crate) fn fixture(
        enrollment: VerifiedManagementEnrollment,
        child: String,
        assignment: String,
        generation: u64,
        expires: Instant,
    ) -> Self {
        Self {
            enrollment,
            child,
            assignment,
            generation,
            expires,
        }
    }
}

/// Only a verified period writer may establish this original assignment before
/// the Agent's first process. Reuse the ordinary pin writer after new credit is
/// inserted in the same transaction; no history or mismatched pin is replaced.
pub(super) fn bind_period_assignment(
    db:&Connection, enrollment:&VerifiedManagementEnrollment, child:&str,
    assignment:&str, generation:u64,
)->Result<(),Error>{
    authority::assignment_for_period(db,enrollment,child,assignment,generation)
}
impl ManagedCallAuthority {
    fn check(&self, registry: &Inner) -> Result<(), Error> {
        self.assignment.check(registry)?;
        identifier(&self.execution, 128)?;
        hash(&self.execution_digest)?;
        // Zero is a postpaid call. Positive legacy estimates remain part of
        // their original immutable identity, never a cap on actual usage.
        if self.estimated_tokens > JSON_SAFE_MAX { return Err(Error::Capacity); }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn fixture(
        assignment: ManagedAssignmentAuthority,
        execution: String,
        execution_digest: String,
        estimated_tokens: u64,
    ) -> Self {
        Self {
            assignment,
            execution,
            execution_digest,
            estimated_tokens,
        }
    }
}
impl ManagedCleanupAuthority {
    /// Restrict cleanup to an authenticated delivery's monotonic lease. This
    /// composes with the original native/profile fence, including queued writes.
    pub fn restrict_to_deadline(mut self, deadline: Instant) -> Result<Self, Error> {
        if deadline <= Instant::now() { return Err(Error::LocalAuthority); }
        self.expires=self.expires.min(deadline);
        let prior=self.enrollment.profile.clone();
        let expires=self.expires;
        self.enrollment.profile.native_gate=Some(Arc::new(move||prior.check().is_ok()&&Instant::now()<expires));
        Ok(self)
    }
    fn check(&self, registry: &Inner) -> Result<(), Error> {
        self.enrollment.check()?;
        identifier(&self.child, 128)?;
        if registry.closed.load(Ordering::Acquire) || Instant::now() >= self.expires {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
    fn deadline(&self) -> Instant {
        self.expires.min(self.enrollment.expires)
    }
    #[cfg(test)]
    pub(crate) fn fixture(
        enrollment: VerifiedManagementEnrollment,
        child: String,
        expires: Instant,
    ) -> Self {
        Self {
            enrollment,
            child,
            expires,
        }
    }
}

/// Private, cloneable queue representation. Public custody handles do not
/// implement Clone/Deserialize, and exact history can never recreate one.
#[derive(Clone)]
struct Scope {
    registry: Arc<Inner>,
    profile: ProfileAuthority,
    child: String,
    target: String,
    binding: String,
    issued_revision: u64,
    expires: Instant,
}
impl Scope {
    /// Registers this call as owned by a live custody object in this process.
    fn mark_live(&self) {
        if let Ok(mut live) = self.registry.live_calls.lock() {
            live.insert(self.target.clone());
        }
    }
    fn unmark_live(&self) {
        if let Ok(mut live) = self.registry.live_calls.lock() {
            live.remove(&self.target);
        }
    }
    fn check(&self, registry: &Arc<Inner>) -> Result<(), Error> {
        self.profile.check()?;
        if !Arc::ptr_eq(&self.registry, registry)
            || registry.closed.load(Ordering::Acquire)
            || Instant::now() >= self.expires
        {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
}
fn after(profile: &ProfileAuthority, registry: &Inner, deadline: Instant) -> Result<(), Error> {
    if profile.check().is_err()
        || registry.closed.load(Ordering::Acquire)
        || Instant::now() >= deadline
    {
        Err(Error::OutcomeUnknown)
    } else {
        Ok(())
    }
}
