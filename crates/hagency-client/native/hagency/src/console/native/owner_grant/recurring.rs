//! Native execution of the standing consent recorded by a published V4 grant.
//! A timer never invents terms: every successor retains the owner's approved
//! amount, calendar, eligibility and identity and uses the normal atomic writer
//! and publication/recovery journals. Legacy grants are never opted in.
use super::*;

pub(super) fn inherits(previous: &Original, next: &Original) -> bool {
    let (a, b) = (&previous.selection, &next.selection);
    matches!(previous.receipt_version, 4 | 5 | 6)
        && matches!(next.receipt_version, 4 | 5 | 6)
        && a.calendar.is_some()
        && a.calendar == b.calendar
        && a.allocated_tokens == b.allocated_tokens
        && a.per_request_tokens == b.per_request_tokens
        && a.renewal_mode == b.renewal_mode
        && a.coordinator_mxid == b.coordinator_mxid
        && a.eligible_managers == b.eligible_managers
        && a.allow_self_approval == b.allow_self_approval
        && previous.binding == next.binding
        && admission::AdmissionRound::same_request(&previous.admission, &next.admission)
        && next.period.start_ms >= previous.period.end_ms
        && next.selection.delegation_expires_at_ms == next.period.end_ms
        && next.succession.as_ref().is_some_and(|s| {
            s.kind == OwnerGrantSuccessionKind::Renewal
                && s.predecessor_intent_id == previous.intent_id
                && s.predecessor_grant_id == previous.selection.grant_id
        })
}

impl OwnerGrantAccess {
    /// Drive at most one current-period renewal per published pool. This is a
    /// native host API, not a script approval or a new owner's consent flow.
    pub async fn renew_recurring(&self) -> Result<usize, NativeError> {
        let service = self.service();
        let records = service.recurring_records().await?;
        let mut contributions = std::collections::BTreeSet::new();
        for r in &records {
            if r.original.selection.calendar.is_some() && r.state != State::Declined {
                contributions.insert(r.original.binding.contribution_id.clone());
            }
        }
        let mut renewed = 0;
        let mut failure = None;
        for contribution in contributions {
            if self.retired() {
                return Err(super::failure(401, "management_scope_changed"));
            }
            match self.renew_recurring_pool(&contribution, &records).await {
                Ok(changed) => renewed += usize::from(changed),
                Err(error) => failure = Some(error),
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(renewed)
    }

    async fn renew_recurring_pool(
        &self,
        contribution: &str,
        records: &[Record],
    ) -> Result<bool, NativeError> {
        let now = now_ms()?;
        // A crash after review, local funding or publication retries the exact
        // journaled command. It never creates a second hold for that period.
        // The journal permits only one unresolved successor and appends in
        // order. Revisit its latest entry, not every historical day's network
        // publication on each daemon pass. published_head still checks the
        // complete native chain before creating any successor.
        for record in records
            .iter()
            .rev()
            .filter(|r| {
                r.original.binding.contribution_id == contribution && r.state != State::Declined
            })
            .take(1)
        {
            let original = &record.original;
            let Some(link) = &original.succession else {
                continue;
            };
            // Expiry recovery is independent of standing-consent inheritance.
            // A manual amendment may change terms, but its abandoned review
            // must not block all later renewals. Recover/retire its exact old
            // command without approving or publishing those changed terms.
            if original.selection.calendar.is_some() && now >= original.period.end_ms {
                if record.publication.as_ref().is_some_and(|p| p.confirmed) {
                    // A reply can be lost after commit. Recover the exact
                    // server result even after its period has ended, so the
                    // next renewal can follow the actual chain head.
                    if original.selection.renewal_mode == Some(hagency_core::recurring_quota::RenewalMode::Owner)
                        && !record.publication.as_ref().is_some_and(|p| p.confirmed)
                    {
                        continue;
                    }
                    let observed = self.observe_publication(original.intent_id.clone()).await?;
                    if !observed.is_some_and(|p| {
                        matches!(
                            p.state,
                            OwnerGrantPublicationState::ServerObserved
                                | OwnerGrantPublicationState::Rejected
                        )
                    }) {
                        self.service()
                            .expire_recurring_publication(&self.registry, &original.intent_id)
                            .await?;
                    }
                } else {
                    self.service()
                        .expire_unpublished_recurring(&self.registry, &original.intent_id)
                        .await?;
                }
                continue;
            }
            let Some(previous) = records
                .iter()
                .find(|r| r.original.intent_id == link.predecessor_intent_id)
            else {
                continue;
            };
            if !inherits(&previous.original, original)
                || previous.state != State::Recorded
                || !previous.publication.as_ref().is_some_and(|p| p.confirmed)
            {
                continue;
            }
            if original.selection.renewal_mode == Some(hagency_core::recurring_quota::RenewalMode::Owner)
                && !record.publication.as_ref().is_some_and(|p| p.confirmed) { continue; }
            let observed = self.observe_publication(original.intent_id.clone()).await?;
            if observed.as_ref().is_some_and(|p| {
                matches!(
                    p.state,
                    OwnerGrantPublicationState::ServerObserved
                        | OwnerGrantPublicationState::Rejected
                )
            }) {
                continue;
            }
            // Recheck the predecessor's canonical publication before consuming
            // standing consent. Revoked/withdrawn supply cannot be renewed.
            if !self
                .observe_publication(previous.original.intent_id.clone())
                .await?
                .is_some_and(|p| {
                    matches!(p.state, OwnerGrantPublicationState::ServerObserved)
                        && p.server_state.as_deref() == Some("active")
                })
            {
                continue;
            }
            match self.review(original.intent_id.clone()).await? {
                OwnerGrantReviewResult::Review(review) => {
                    self.confirm(*review).await?;
                }
                OwnerGrantReviewResult::Progress(progress)
                    if matches!(progress.state, OwnerGrantState::Recorded) => {}
                _ => return Ok(false),
            }
            return self.publish_recurring(original.intent_id.clone()).await;
        }
        let service = self.service();
        let cookie = service.cookie().await?;
        let io = service.io(&cookie, Permission::ConsentGrants).await?;
        if !service.supports_calendar(&io).await? {
            return Ok(false);
        }
        // No recorded V4 publication, or a queued manual amendment, is not an
        // automatic renewal candidate. The explicit review remains in control.
        let head = match service
            .published_head(&io, &self.registry, contribution)
            .await
        {
            Ok(head) => head,
            Err(error)
                if matches!(
                    error.code.as_str(),
                    "owner_grant_predecessor_required"
                        | "owner_grant_succession_pending"
                        | "owner_grant_successor_exists"
                ) =>
            {
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        let p = &head.receipt;
        let Some(calendar) = &p.calendar else {
            return Ok(false);
        };
        if p.renewal_mode == Some(hagency_core::recurring_quota::RenewalMode::Owner) { return Ok(false); }
        if now < p.period_end_ms {
            return Ok(false);
        }
        let period = calendar.period_at(now).map_err(|_| history_error())?;
        let choices = OwnerGrantChoices { renewal_mode: p.renewal_mode, per_request_tokens: p.per_request_tokens,
            calendar: Some(calendar.clone()),
            coordinator_mxid: p.coordinator_mxid.clone(),
            delegation_expires_at_ms: period.end_ms,
            allow_self_approval: p.allow_self_approval,
            eligible_managers: p.eligible_managers.clone(),
            allocated_tokens: p.allocated_tokens,
        };
        let review = self
            .prepare_successor(
                contribution.into(),
                OwnerGrantSuccessionKind::Renewal,
                choices,
            )
            .await?;
        if !inherits(&head.original, &review.original) {
            return Err(history_error());
        }
        let id = review.original.intent_id.clone();
        let progress = self.confirm(review).await?;
        if !matches!(progress.state, OwnerGrantState::Recorded) {
            return Ok(false);
        }
        self.publish_recurring(id).await
    }

    async fn publish_recurring(&self, id: String) -> Result<bool, NativeError> {
        let progress = match self.review_publication(id).await? {
            OwnerGrantPublicationResult::Review(review) => {
                self.confirm_publication(*review).await?
            }
            OwnerGrantPublicationResult::Progress(progress) => *progress,
        };
        Ok(
            matches!(progress.state, OwnerGrantPublicationState::ServerObserved)
                && progress.server_state.as_deref() == Some("active"),
        )
    }
}
impl OwnerGrantIntents {
    async fn recurring_records(&self) -> Result<Vec<Record>, NativeError> {
        let cookie = self.cookie().await?;
        let io = self.io(&cookie, Permission::ConsentGrants).await?;
        let _lane = self.manager.owner.0.management.mutations.lock().await;
        io.check().await?;
        Ok(History::load(&io.path, io.identity.clone())?.data.records)
    }
    /// An unconfirmed expired successor cannot ever be published. Reconcile
    /// its local result before retiring only its unused review/hold. Confirmed
    /// publication commands must be recovered from the server, never guessed.
    async fn expire_unpublished_recurring(
        &self,
        registry: &OwnerResourceRegistry,
        id: &str,
    ) -> Result<(), NativeError> {
        let cookie = self.cookie().await?;
        let io = self.io(&cookie, Permission::ConsentGrants).await?;
        let _lane = self.manager.owner.0.management.mutations.lock().await;
        let mut history = History::load(&io.path, io.identity.clone())?;
        let index = history.index(id)?;
        let r = &history.data.records[index];
        if r.original.period.end_ms > now_ms()?
            || r.publication.as_ref().is_some_and(|p| p.confirmed)
        {
            return Ok(());
        }
        let original = r.original.clone();
        let progress = self.recovered(&io, registry, &mut history, index).await?;
        match progress.state {
            OwnerGrantState::Recorded => {
                if progress.succession_state == Some(OwnerGrantSuccessionState::Pending) {
                    registry
                        .reject_capacity_grant_succession(
                            io.profile.clone(),
                            io.identity.device.clone(),
                            original.selection.grant_id,
                        )
                        .await
                        .map_err(storage)?;
                }
            }
            OwnerGrantState::ReviewExpired | OwnerGrantState::OutcomeUnknown => {
                // The authoritative recovery above found no receipt; the
                // expired period check prevents every queued writer from issuing.
                history.data.records[index].state = State::Declined;
                history.save()?;
            }
            _ => {}
        }
        io.check().await
    }
}
