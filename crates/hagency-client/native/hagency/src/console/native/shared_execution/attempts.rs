//! Explicit native successor control. Ordinary polls never create start intents.
use super::*;
impl SharedExecutionTransport {
    /// Called once for an accepted native startup/explicit start. Restart finds
    /// the same original intent before considering a new request identifier.
    pub async fn start_successor(
        &self,
        r: &OwnerResourceRegistry,
        original: SharedRuntimeCommand,
        predecessor: Option<SharedAttemptCommand>,
    ) -> Result<SharedExecutionProgress, NativeError> {
        self.start_successor_scoped(r, original, predecessor, None)
            .await
    }
    pub(super) async fn start_successor_scoped(
        &self,
        r: &OwnerResourceRegistry,
        original: SharedRuntimeCommand,
        predecessor: Option<SharedAttemptCommand>,
        caller_gate: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    ) -> Result<SharedExecutionProgress, NativeError> {
        if !cfg!(feature = "shared-successors") {
            return Err(failure(409, "shared_successors_disabled"));
        }
        let mut scoped_io = self.io.clone();
        if let Some(gate) = &caller_gate {
            scoped_io.restrict_to_host(gate.clone());
        }
        let b = scoped_io.bound().await?;
        let cleanup = b.cleanup(r, &original).await?;
        let saved = r
            .recover_shared_attempt_start(cleanup, original.clone(), predecessor.clone())
            .await
            .map_err(store)?;
        let intent = if let Some(saved) = saved {
            saved
        } else {
            let enrollment = b.enrollment(r, &original, true).await?;
            let mut bytes = [0u8; 16];
            getrandom::fill(&mut bytes).map_err(|_| failure(503, "native_random_unavailable"))?;
            let id = format!(
                "sasi_{}",
                bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
            );
            r.prepare_shared_attempt_start(enrollment, original, predecessor, id)
                .await
                .map_err(store)?
        };
        b.transport.check_scope(&b.scope, false).await?;
        self.process_attempt_scoped(
            r,
            attempt_io::AttemptAction::Start(intent.input().clone()),
            None,
            None,
            caller_gate,
        )
        .await
    }
    pub async fn poll_successor(
        &self,
        r: &OwnerResourceRegistry,
        after: Option<String>,
    ) -> Result<SharedExecutionProgress, NativeError> {
        self.process_attempt(r, attempt_io::AttemptAction::Poll { after }, None, None)
            .await
    }
    pub async fn recover_successor(
        &self,
        r: &OwnerResourceRegistry,
        id: String,
        command_digest: String,
        observation: Option<String>,
    ) -> Result<SharedExecutionProgress, NativeError> {
        self.process_attempt(
            r,
            attempt_io::AttemptAction::Recover {
                id: id.clone(),
                digest: command_digest.clone(),
                observation: observation.clone(),
            },
            Some((id, command_digest, observation)),
            None,
        )
        .await
    }
    #[cfg(all(test, feature = "shared-successors"))]
    pub(in crate::console::native) async fn poll_successor_fixture(
        &self,
        r: &OwnerResourceRegistry,
        launch: super::super::managed_runtime::PreparedLaunch,
    ) -> Result<SharedExecutionProgress, NativeError> {
        self.process_attempt(
            r,
            attempt_io::AttemptAction::Poll { after: None },
            None,
            Some(launch),
        )
        .await
    }
    #[cfg(all(test, feature = "shared-successors"))]
    pub(in crate::console::native) async fn poll_claude_successor_fixture(
        &self,
        r: &OwnerResourceRegistry,
        resource_id: String,
        root: std::path::PathBuf,
        binding: hagency_runtime::claude::account::AccountBinding,
        mode: &str,
    ) -> Result<SharedExecutionProgress, NativeError> {
        let b = self.io.bound().await?;
        let resource = r
            .resource(&b.scope.profile, resource_id)
            .await
            .map_err(store)?;
        let launch = launch::ClaudeLaunch::fixture(
            r.clone(),
            b.scope.profile,
            &resource,
            root,
            binding,
            mode,
        );
        self.process_attempt_scoped(
            r,
            attempt_io::AttemptAction::Poll { after: None },
            None,
            Some(launch::Launch::Claude(launch)),
            None,
        )
        .await
    }
    #[cfg(all(test, feature = "shared-successors"))]
    pub(in crate::console::native) async fn poll_octos_successor_fixture(
        &self, r: &OwnerResourceRegistry, resource_id: String, home: std::path::PathBuf,
        root: std::path::PathBuf, profile_id: String,
    ) -> Result<SharedExecutionProgress, NativeError> {
        let b = self.io.bound().await?;
        let resource = r.resource(&b.scope.profile, resource_id).await.map_err(store)?;
        let launch = launch::OctosLaunch::fixture(r.clone(), b.scope.profile, &resource, home, root, profile_id);
        self.process_attempt_scoped(r, attempt_io::AttemptAction::Poll { after: None }, None,
            Some(launch::Launch::Octos(launch)), None).await
    }
    pub(super) async fn process_attempt(
        &self,
        r: &OwnerResourceRegistry,
        action: attempt_io::AttemptAction,
        historical: Option<(String, String, Option<String>)>,
        prepared: Option<super::super::managed_runtime::PreparedLaunch>,
    ) -> Result<SharedExecutionProgress, NativeError> {
        self.process_attempt_scoped(r, action, historical, prepared.map(Into::into), None)
            .await
    }
    async fn process_attempt_scoped(
        &self,
        r: &OwnerResourceRegistry,
        action: attempt_io::AttemptAction,
        historical: Option<(String, String, Option<String>)>,
        prepared: Option<launch::Launch>,
        caller_gate: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    ) -> Result<SharedExecutionProgress, NativeError> {
        if !cfg!(feature = "shared-successors") {
            return Err(failure(409, "shared_execution_disabled"));
        }
        let expected_start = match &action {
            attempt_io::AttemptAction::Start(input) => Some(input.clone()),
            attempt_io::AttemptAction::DemandStart { input, .. } => Some(input.start.clone()),
            _ => None,
        };
        let b = self.io.bound().await?;
        let started = Instant::now();
        // The package gate covers the initial POST and its asynchronous result,
        // not the accepted process lifetime or original background reconciliation.
        let reply: Timed<attempt_io::Reply> = if let Some(gate) = caller_gate {
            let mut io = self.io.clone();
            io.restrict_to_host(gate);
            io.bound().await?.request(action.into(), false).await?
        } else {
            b.request(action.into(), false).await?
        };
        let Some(d) = reply.value.delivery else {
            return if historical.is_none() {
                Ok(SharedExecutionProgress::Idle)
            } else {
                Err(invalid())
            };
        };
        d.command
            .validate(b.scope.device.owner_mxid())
            .map_err(store)?;
        io::binding(
            &b.scope,
            &d.command.original_runtime_command,
            d.execution_allowed,
        )?;
        if d.command_digest != digest(&d.command)?
            || historical
                .as_ref()
                .is_some_and(|(id, h, _)| id != &d.command.command_id || h != &d.command_digest)
            || historical.is_some() && d.execution_allowed
            || d.recorded.is_some() && d.execution_allowed
            || d.execution_allowed && d.cleanup_allowed
            || historical.is_some() && d.cleanup_allowed
        {
            return Err(invalid());
        }
        if let Some(input) = expected_start
            && (d.command.start_intent_id != input.start_intent_id
                || d.command.assignment_id != input.assignment_id
                || digest(&d.command.predecessor)? != digest(&input.predecessor)?
                || digest(&d.command.budget_observation)? != digest(&input.budget_observation)?)
        {
            return Err(invalid());
        }
        let until = io::window(
            &d.token,
            d.lease_until_ms,
            reply.observed,
            started,
            &b.scope,
        )?;
        let c = &d.command;
        if let Some(saved) = &d.recorded {
            saved.validate_for(c).map_err(store)?;
            if historical
                .as_ref()
                .and_then(|v| v.2.as_deref())
                .is_some_and(|id| id != saved.observation_id)
            {
                return Err(invalid());
            }
        }
        if historical.is_none() && d.cleanup_allowed {
            let authority = b
                .cleanup(r, &c.original_runtime_command)
                .await?
                .restrict_to_deadline(until)
                .map_err(store)?;
            r.fence_shared_successor(authority, c.clone())
                .await
                .map_err(store)?;
            b.transport.check_scope(&b.scope, false).await?;
        }

        if historical.is_none()
            && d.cleanup_allowed
            && self
                .manager
                .owner
                .0
                .shared_execution
                .request_stop(&c.command_id, &d.command_digest)?
        {
            return Ok(SharedExecutionProgress::Stopping {
                command_id: c.command_id.clone(),
            });
        }
        if historical.is_none()
            && self
                .manager
                .owner
                .0
                .shared_execution
                .running(&c.command_id, &d.command_digest)?
        {
            return Ok(SharedExecutionProgress::Running {
                command_id: c.command_id.clone(),
            });
        }
        // Only read-only history may advance without local evidence, after
        // the original live-process checks. Cleanup/execution tickets retain
        // their strict proof path and cannot skip a fence, stop or claim.
        let a = if !d.execution_allowed && !d.cleanup_allowed {
            let Some(authority) = b.find_cleanup(r, &c.original_runtime_command).await? else {
                return Ok(SharedExecutionProgress::HistoricalEvidenceMissing {
                    command_id: c.command_id.clone(),
                });
            };
            authority
        } else {
            b.cleanup(r, &c.original_runtime_command).await?
        };
        // An unclaimed fenced attempt already yields its not_started receipt.
        let mut local = r
            .recover_shared_attempt_process(
                a,
                c.clone(),
                historical.as_ref().and_then(|v| v.2.clone()),
            )
            .await
            .map_err(store)?;
        // Time bound (ADR-193): a claimed process with no stop evidence and no
        // live local child is interrupted once its grace period has passed.
        let cleaning = historical.is_none() && d.cleanup_allowed && !d.execution_allowed;
        if cleaning
            && local
                .as_ref()
                .is_none_or(|l| !matches!(l.state.as_str(), "stopped" | "not_started"))
            && let Some(timeout) = r
                .record_shared_attempt_timeout(
                    b.cleanup(r, &c.original_runtime_command).await?,
                    c.clone(),
                )
                .await
                .map_err(store)?
        {
            local = Some(timeout);
        }
        b.transport.check_scope(&b.scope, false).await?;
        if let Some(saved) = &d.recorded {
            if let Some(latest) = &local {
                if latest.ledger_revision > saved.ledger_revision && latest.reason.is_some() {
                    // A time-bound end is accepted only on a cleanup ticket;
                    // otherwise a later cleanup poll reports it.
                    if !cleaning || !attempt_run::upload_end(&b, &d, until, latest).await? {
                        return Ok(SharedExecutionProgress::HistoricalEvidenceMissing {
                            command_id: c.command_id.clone(),
                        });
                    }
                    attempt_run::recover_process_work(&self.io, r, c).await?;
                    return Ok(SharedExecutionProgress::Recorded {
                        command_id: c.command_id.clone(),
                        observation_id: latest.observation_id.clone(),
                        process_state: Some(latest.state.clone()),
                    });
                }
                if latest.ledger_revision > saved.ledger_revision {
                    attempt_run::report_process(&self.io, r, c, latest).await?;
                    attempt_run::recover_process_work(&self.io, r, c).await?;
                    return Ok(SharedExecutionProgress::Recorded {
                        command_id: c.command_id.clone(),
                        observation_id: latest.observation_id.clone(),
                        process_state: Some(latest.state.clone()),
                    });
                }
                if latest.ledger_revision == saved.ledger_revision
                    && digest(latest)? != digest(saved)?
                {
                    return Err(invalid());
                }
            }
            if local.as_ref().is_some_and(|l| {
                l.observation_id == saved.observation_id && digest(l).ok() != digest(saved).ok()
            }) {
                return Err(invalid());
            }
            if local.is_some() {
                attempt_run::recover_process_work(&self.io, r, c).await?;
            }
            return Ok(SharedExecutionProgress::Recorded {
                command_id: c.command_id.clone(),
                observation_id: saved.observation_id.clone(),
                process_state: Some(saved.state.clone()),
            });
        }
        if let Some(local) = local {
            // Non-issuance and time-bound ends are accepted only on a cleanup
            // ticket; a later cleanup poll reports them.
            if !cleaning && (local.state == "not_started" || local.reason.is_some()) {
                return Ok(SharedExecutionProgress::HistoricalEvidenceMissing {
                    command_id: c.command_id.clone(),
                });
            }
            // A never-started attempt of a demand asks the server to run the
            // same event again, with this evidence (ADR-193 amendment).
            if cleaning
                && local.state == "not_started"
                && attempt_run::upload_retry(&b, r, &d, until, &local).await?
            {
                return Ok(SharedExecutionProgress::Recorded {
                    command_id: c.command_id.clone(),
                    observation_id: local.observation_id,
                    process_state: Some(local.state.clone()),
                });
            }
            // Report actual stop independently before accounting recovery: an
            // old work upload failure must not hide known process termination.
            if !attempt_run::upload_end(&b, &d, until, &local).await? {
                return Ok(SharedExecutionProgress::HistoricalEvidenceMissing {
                    command_id: c.command_id.clone(),
                });
            }
            attempt_run::recover_process_work(&self.io, r, c).await?;
            return Ok(SharedExecutionProgress::Recorded {
                command_id: c.command_id.clone(),
                observation_id: local.observation_id,
                process_state: Some(local.state.clone()),
            });
        }
        if !d.execution_allowed {
            return Ok(SharedExecutionProgress::HistoricalEvidenceMissing {
                command_id: c.command_id.clone(),
            });
        }
        let e = b.enrollment(r, &c.original_runtime_command, true).await?;
        let proof =
            VerifiedSharedAttemptDelivery::new(e, c.clone(), d.command_digest.clone(), true, until)
                .map_err(store)?;
        let a = b.cleanup(r, &c.original_runtime_command).await?;
        if r.shared_successor_history(a, c.clone())
            .await
            .map_err(store)?
            .is_some()
        {
            return Ok(SharedExecutionProgress::HistoricalEvidenceMissing {
                command_id: c.command_id.clone(),
            });
        }
        let a = b.cleanup(r, &c.original_runtime_command).await?;
        if !r
            .require_shared_attempt_start(a, c.clone())
            .await
            .map_err(store)?
        {
            return Ok(SharedExecutionProgress::HistoricalEvidenceMissing {
                command_id: c.command_id.clone(),
            });
        }
        let resource = r
            .resource(
                &b.scope.profile,
                c.original_runtime_command
                    .reserve_command
                    .resource_id
                    .clone(),
            )
            .await
            .map_err(store)?;
        if resource.framework != c.original_runtime_command.runtime().map_err(store)?
            || resource.revision != c.original_runtime_command.reserve_command.resource_revision
        {
            return Err(failure(409, "shared_execution_resource_changed"));
        }
        let launch = if let Some(prepared) = prepared {
            prepared
        } else {
            let root = self
                .manager
                .owner
                .0
                .host
                .console
                .0
                .server_login
                .state_directory()
                .ok_or_else(|| failure(409, "shared_execution_launch_unavailable"))?;
            super::launch::Launch::for_resource(
                &root,
                &b.scope.device,
                r,
                &b.scope.profile,
                &resource,
            )
            .await?
        };
        let launch = launch.with_live_web(c.web_search)?.with_agent_tools(
            &c.original_runtime_command.reserve_command.proposed_agent_id,
            c.tool_policy.as_ref().map(|p| p.mode.as_str()),
        )?;
        let process_io = launch.io(&self.io);
        let io = process_io.clone();
        let command = c.clone();
        let check: super::super::managed_runtime::LiveCheck = Arc::new(move || {
            let io = io.clone();
            let c = command.clone();
            Box::pin(async move { io.check(&c.original_runtime_command).await })
        });
        let launch = launch.prepare(&self.manager, until, check).await?;
        b.transport.check_scope(&b.scope, false).await?;
        self.manager
            .owner
            .0
            .shared_execution
            .start_attempt(
                r.clone(),
                proof,
                launch,
                process_io,
                d,
                reply.observed,
                started,
                until,
            )?
            .await
            .map_err(|_| failure(409, "shared_execution_outcome_unknown"))?
    }
}
