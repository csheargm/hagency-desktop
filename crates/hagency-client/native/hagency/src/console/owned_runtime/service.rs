//! One explicit service intent per Agent. Room workers retain their own context,
//! authorization and ledger. Discovery never carries credentials or tool grants.
use super::*;

pub(super) const ALL_ROOMS: &str = "*";
pub(super) struct ServiceEntry {
    pin: DevicePin,
    gate: Arc<Mutex<()>>,
    cancel: watch::Sender<bool>,
    task: RetainedTask,
    health: watch::Receiver<ServiceHealth>,
}
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceHealth {
    phase: &'static str,
    last_error: Option<&'static str>,
    problems: Vec<serde_json::Value>,
}
struct RetryState {
    failures: u8,
    next: Instant,
    error: &'static str,
}
fn schedule_retry(
    retries: &mut BTreeMap<String, RetryState>,
    binding: &str,
    error: &'static str,
    at: Instant,
) {
    let failures = retries
        .get(binding)
        .map_or(1, |retry| retry.failures.saturating_add(1));
    let seconds = 2u64.saturating_pow(u32::from(failures.min(6))).min(60);
    retries.insert(
        binding.into(),
        RetryState {
            failures,
            next: at + Duration::from_secs(seconds),
            error,
        },
    );
}

impl ServiceEntry {
    pub(super) fn profile_matches(&self, pin: &DevicePin) -> bool {
        self.pin.origin == pin.origin
            && self.pin.issuer == pin.issuer
            && self.pin.subject == pin.subject
            && self.pin.owner == pin.owner
            && self.pin.user == pin.user
            && !self.task.is_finished()
    }
    pub(super) fn cancel_now(&self) {
        self.cancel.send_replace(true);
    }
}

/// Non-authoritative UI/start preferences. These contain no enabled flag,
/// consent, credential, lease or path, and are never read by recovery.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::console) struct ServiceOptions {
    pub agent_id: String,
    pub device_id: String,
    pub effort: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SavedOptions {
    version: u8,
    profile_identity: String,
    options: Vec<ServiceOptions>,
}
fn read_options(path: &std::path::Path, identity: &str) -> Result<SavedOptions, RuntimeError> {
    use std::io::Read;
    let file = match hagency_store::private::open(path, false) {
        Ok(file) => file,
        Err(_) if !path.exists() => {
            return Ok(SavedOptions {
                version: RUNTIME_STATE_VERSION,
                profile_identity: identity.into(),
                options: Vec::new(),
            });
        }
        Err(_) => return Err(RuntimeError::Profile),
    };
    let mut bytes = Vec::new();
    file.take(262145)
        .read_to_end(&mut bytes)
        .map_err(|_| RuntimeError::Profile)?;
    if bytes.len() > 262144 {
        return Err(RuntimeError::Profile);
    }
    let Some(value) = current_runtime_state(&bytes, identity)? else {
        return Ok(SavedOptions { version: RUNTIME_STATE_VERSION, profile_identity: identity.into(), options: Vec::new() });
    };
    let values: SavedOptions = serde_json::from_value(value).map_err(|_| RuntimeError::Profile)?;
    let mut unique = BTreeSet::new();
    if values.version != RUNTIME_STATE_VERSION
        || values.profile_identity != identity
        || values.options.len() > 1024
        || values.options.iter().any(|option| {
            option.agent_id.is_empty()
                || option.device_id.is_empty()
                || ![
                    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
                ]
                .contains(&option.effort.as_str())
                || !unique.insert((&option.agent_id, &option.device_id))
        })
    {
        return Err(RuntimeError::Profile);
    }
    Ok(values)
}
fn options_for_device(values: SavedOptions, agent: &str, device: &str) -> Option<ServiceOptions> {
    values
        .options
        .into_iter()
        .find(|option| option.agent_id == agent && option.device_id == device)
}
fn save_options(
    path: &std::path::Path,
    identity: &str,
    option: ServiceOptions,
) -> Result<(), RuntimeError> {
    let lock_path = path.with_file_name("agent-service-options.lock");
    let lock = hagency_store::private::open(&lock_path, true)
        .or_else(|_| hagency_store::private::open(&lock_path, false))
        .map_err(|_| RuntimeError::Profile)?;
    lock.try_lock().map_err(|_| RuntimeError::Profile)?;
    let mut values = read_options(path, identity)?;
    values
        .options
        .retain(|value| value.agent_id != option.agent_id || value.device_id != option.device_id);
    values.options.push(option);
    let bytes = serde_json::to_vec(&values).map_err(|_| RuntimeError::Profile)?;
    if bytes.len() > 262144 || values.options.len() > 1024 {
        return Err(RuntimeError::Profile);
    }
    hagency_store::private::replace(path, &bytes).map_err(|_| RuntimeError::Profile)
}

pub(super) fn owner_error(error: super::super::server_login::OwnerError) -> RuntimeError {
    if matches!(error.status, 401 | 403 | 404 | 409) {
        RuntimeError::Authorization
    } else {
        RuntimeError::Transport
    }
}

fn remove_agent_intents(
    path: &std::path::Path,
    identity: &str,
    agent: &str,
) -> Result<(), RuntimeError> {
    let lock_path = path.with_file_name("runtime-intents.lock");
    let lock = hagency_store::private::open(&lock_path, true)
        .or_else(|_| hagency_store::private::open(&lock_path, false))
        .map_err(|_| RuntimeError::Profile)?;
    lock.try_lock().map_err(|_| RuntimeError::Profile)?;
    let mut values = read_intents(path, identity)?;
    values.intents.retain(|intent| intent.agent_id != agent);
    hagency_store::private::replace(
        path,
        &serde_json::to_vec(&values).map_err(|_| RuntimeError::Profile)?,
    )
    .map_err(|_| RuntimeError::Profile)
}
fn service_enabled(
    path: &std::path::Path,
    identity: &str,
    intent: &RuntimeIntent,
) -> Result<bool, RuntimeError> {
    Ok(read_intents(path, identity)?
        .intents
        .iter()
        .any(|value| value == intent))
}
fn active_binding_ids(bindings: &[Binding], agent: &str) -> BTreeSet<String> {
    bindings
        .iter()
        .filter(|binding| binding.agent_id == agent && binding.state == "active")
        .map(|binding| binding.id.clone())
        .collect()
}

/// The profile must be this owner's own connection to the coding tool the
/// Agent uses: the same checks as a single Room start (`OwnedRuntime::start`).
/// Comparing with the owner's Codex account alone refused every Claude Code
/// and Octos Agent with "runtime_profile_unavailable".
fn owner_connection(
    root: &std::path::Path,
    origin: &str,
    issuer: &str,
    subject: &str,
    owner: &str,
    config: &StartConfig,
) -> Result<(), RuntimeError> {
    use super::super::owner_provider::{external, selected_reference};
    runtime::validate(&config.profile, &config.credential_ref)
        .map_err(|_| RuntimeError::Profile)?;
    let expected = external::paths_for_reference(
        root,
        origin,
        issuer,
        subject,
        owner,
        &config.credential_ref,
    )
    .map_err(|_| RuntimeError::Profile)?;
    if config.profile.executable
        != external::executable(runtime::kind(&config.credential_ref))
            .map_err(|_| RuntimeError::Profile)?
        || config.profile.codex_home != expected.codex_home
        || config.profile.home != expected.home
        || config.profile.shared_auth != expected.shared
        || config.credential_ref != expected.credential_ref
    {
        return Err(RuntimeError::Profile);
    }
    if runtime::managed_reference(&config.credential_ref).is_some() {
        external::verify_reference(&config.profile.codex_home, &config.credential_ref)
            .map_err(|_| RuntimeError::Profile)?;
    } else if selected_reference(&config.profile.codex_home, config.profile.shared_auth)
        .map_err(|_| RuntimeError::Profile)?
        != config.credential_ref
    {
        return Err(RuntimeError::Profile);
    }
    Ok(())
}

impl OwnedRuntime {
    pub(in crate::console) async fn saved_service_options(
        &self,
        console: &Console,
        agent: &str,
    ) -> Result<Option<ServiceOptions>, RuntimeError> {
        let device = console.authorized_device().await.map_err(device_error)?;
        device.bearer().map_err(|_| RuntimeError::Authorization)?;
        let (path, identity) = intent_location(console, &device)?;
        Ok(options_for_device(
            read_options(
                &path.with_file_name("agent-service-options.json"),
                &identity,
            )?,
            agent,
            device.device_id(),
        ))
    }
    pub(in crate::console) async fn start_agent_service(
        &self,
        console: Console,
        mut config: StartConfig,
    ) -> Result<serde_json::Value, RuntimeError> {
        let _activity = self.tasks.enter().map_err(|_| RuntimeError::Stopped)?;
        let device = console.authorized_device().await.map_err(device_error)?;
        device.bearer().map_err(|_| RuntimeError::Authorization)?;
        let pin = DevicePin::from(&device);
        let assigned = console
            .owner_api_pinned(
                &pin,
                OwnerOperation::Agent {
                    agent: config.agent_id.clone(),
                },
            )
            .await
            .map_err(owner_error)?;
        verify_execution_device(&assigned.value, &config.agent_id, device.device_id())?;
        if assigned.owner != device.owner_mxid()
            || assigned.value["agent"]["ownerUserId"] != device.user_id()
        {
            return Err(RuntimeError::Authorization);
        }
        config.binding_id = ALL_ROOMS.into();
        // Agent-wide starts do not inherit private Room file access. Shared
        // workspace access is separately selected and applies only to
        // runtimes using Hagency's scoped, owner-approved host file tools.
        if config.host_files || (config.shared_workspace && !matches!(runtime::kind(&config.credential_ref), runtime::Kind::Codex | runtime::Kind::Octos)) {
            return Err(RuntimeError::Profile);
        }
        let root = console
            .0
            .server_login
            .state_directory()
            .ok_or(RuntimeError::Profile)?;
        owner_connection(
            &root,
            device.origin(),
            device.issuer(),
            device.subject(),
            device.owner_mxid(),
            &config,
        )?;
        config.shared_workspace_pin = if config.shared_workspace { Some(shared_workspace_pin(&config.profile.cwd)?) } else { None };
        let (path, profile_id) = intent_location(&console, &device)?;

        let key = identity(&device, &config.agent_id);
        let mut services = self.services.lock().await;
        self.tasks.check_open().map_err(|_| RuntimeError::Stopped)?;
        device.bearer().map_err(|_| RuntimeError::Authorization)?;
        verify_recovery_intent(&console, &device, &config)?;
        if services
            .get(&key)
            .is_some_and(|entry| !entry.task.is_finished() && !*entry.cancel.borrow())
        {
            return Err(RuntimeError::AlreadyActive);
        }
        let intent = RuntimeIntent {
            agent_id: config.agent_id.clone(),
            binding_id: ALL_ROOMS.into(),
            device_id: device.device_id().into(),
            consent_id: config
                .recovery_consent_id
                .clone()
                .unwrap_or(new_consent_id()?),

            effort: config.profile.effort.clone(),
            host_files: false,
            shared_workspace: config.shared_workspace,
            shared_workspace_pin: config.shared_workspace_pin.clone(),
        };
        save_options(
            &path.with_file_name("agent-service-options.json"),
            &profile_id,
            ServiceOptions {
                agent_id: config.agent_id.clone(),
                device_id: device.device_id().into(),

                effort: config.profile.effort.clone(),
            },
        )?;
        change_intent_checked(
            &path,
            &profile_id,
            &config.agent_id,
            ALL_ROOMS,
            Some(intent.clone()),
            config.recovery_consent_id.as_deref(),
        )?;
        config.recovery_device_id = None;
        config.recovery_consent_id = None;
        let (cancel, mut receiver) = watch::channel(false);
        let (health, health_receiver) = watch::channel(ServiceHealth {
            phase: "starting",
            ..Default::default()
        });
        let gate = Arc::new(Mutex::new(()));
        let task_gate = gate.clone();
        let manager = self.clone();
        let host = console.clone();
        let task_pin = pin.clone();
        let agent_id = config.agent_id.clone();
        let task = tokio::spawn(async move {
            let mut attempted = BTreeSet::new();
            let mut retries: BTreeMap<String, RetryState> = BTreeMap::new();
            let mut takeover_consumed = false;
            let mut discovery_failures = 0u8;
            let observed = receiver.clone();
            loop {
                let result = tokio::select! { biased;
                    _ = canceled(&mut receiver) => break,
                    result = async {
                        let _gate = task_gate.lock().await;
                        if *observed.borrow() || !service_enabled(&path, &profile_id, &intent)? { return Err(RuntimeError::Stopped); }
                        let current = host.authorized_device().await.map_err(device_error)?;
                        current.bearer().map_err(|_| RuntimeError::Authorization)?;
                        if DevicePin::from(&current) != task_pin { return Err(RuntimeError::Authorization); }
                        let assigned = host.owner_api_pinned(&task_pin, OwnerOperation::Agent { agent: intent.agent_id.clone() })
                            .await.map_err(owner_error)?;
                        verify_execution_device(&assigned.value, &intent.agent_id, &intent.device_id)?;
                        let reply = host.owner_api_pinned(&task_pin, OwnerOperation::Bindings { agent: intent.agent_id.clone() })
                            .await.map_err(owner_error)?;
                        let bindings: Vec<Binding> = serde_json::from_value(reply.value["bindings"].clone()).map_err(|_| RuntimeError::Profile)?;
                        let active = active_binding_ids(&bindings, &intent.agent_id);
                        // A paused/removed Room must terminate its worker. No local
                        // room intent is allowed to resurrect it on recovery.
                        let snapshot = manager.status(&host, &intent.agent_id).await?;
                        for child in snapshot.as_ref().into_iter().flat_map(|status| &status.bindings) {
                            let Some(binding) = child.binding_id.as_ref() else {continue;};
                            if !active.contains(binding) {
                                manager.stop(&host, &intent.agent_id, binding).await?;
                            } else if child.phase == "stopped" && attempted.remove(binding) {
                                schedule_retry(&mut retries,binding,child.last_error.unwrap_or("room_worker_stopped"),Instant::now());
                            } else if matches!(child.phase, "online_chat_only" | "online") {
                                retries.remove(binding);
                            }
                        }
                        attempted.retain(|binding| active.contains(binding));
                        retries.retain(|binding,_| active.contains(binding));
                        for binding in active {
                            if attempted.contains(&binding) || retries.get(&binding).is_some_and(|retry| retry.next > Instant::now()) { continue; }
                            let mut child = config.clone(); child.binding_id = binding.clone();
                            // Takeover applies only to the initial Agent lease,
                            // and cannot be repeated because a Room was added.
                            if takeover_consumed { child.takeover = Takeover::Never; }
                            match manager.start(host.clone(), "", child).await {
                                Ok(_) => { attempted.insert(binding); takeover_consumed = true; }
                                Err(RuntimeError::AlreadyActive) => {
                                    // A closing lease/process lock is not an
                                    // already running Room; retry discovery.
                                    if manager.status(&host, &intent.agent_id).await?.is_some_and(|status|
                                        status.phase != "stopped" && status.bindings.iter().any(|child|
                                            child.binding_id.as_deref() == Some(&binding) && child.phase != "stopped")) {
                                        attempted.insert(binding); takeover_consumed = true;
                                    } else {
                                        schedule_retry(&mut retries,&binding,"agent_process_closing",Instant::now());
                                    }
                                }
                                Err(RuntimeError::Stopped | RuntimeError::Authorization | RuntimeError::ExecutionDevice) => return Err(RuntimeError::Authorization),
                                Err(error) => {
                                    let code = failure_code(&Err(error)).unwrap_or("runtime_start_failed");
                                    schedule_retry(&mut retries,&binding,code,Instant::now());
                                }
                            }
                        }
                        health.send_replace(ServiceHealth {phase:if retries.is_empty(){"serving"}else{"retrying"},last_error:None,
                            problems:retries.iter().map(|(binding,retry)|serde_json::json!({"bindingId":binding,"error":retry.error,"retryInSeconds":retry.next.saturating_duration_since(Instant::now()).as_secs()})).collect()});
                        Ok(())
                    } => result
                };
                if result.is_err() {
                    health.send_replace(ServiceHealth {
                        phase: "disconnected",
                        last_error: failure_code(&result),
                        problems: Vec::new(),
                    });
                }
                if matches!(
                    result,
                    Err(RuntimeError::Authorization
                        | RuntimeError::ExecutionDevice
                        | RuntimeError::Stopped)
                ) {
                    break;
                }
                let seconds = if result.is_err() {
                    discovery_failures = discovery_failures.saturating_add(1);
                    2u64.saturating_pow(u32::from(discovery_failures.min(6)))
                        .min(60)
                } else {
                    discovery_failures = 0;
                    2
                };
                tokio::select! { biased; _ = canceled(&mut receiver) => break, _ = tokio::time::sleep(Duration::from_secs(seconds)) => {} }
            }
            let mut final_health = health.borrow().clone();
            final_health.phase = "stopped";
            health.send_replace(final_health);
            // Account/device revocation also drains children; the journal stays
            // pinned for a later authenticated startup, never a bearer replay.
            let _ = manager
                .cancel_agent_entry(&host, &task_pin, &intent.agent_id)
                .await;
        });
        let task = self.tasks.track(task);
        services.insert(
            key,
            ServiceEntry {
                pin,
                gate,
                cancel,
                task,
                health: health_receiver,
            },
        );
        drop(services);
        self.agent_service_status(&console, &agent_id).await
    }
    pub(in crate::console) async fn agent_service_status(
        &self,
        console: &Console,
        agent: &str,
    ) -> Result<serde_json::Value, RuntimeError> {
        let device = console.authorized_device().await.map_err(device_error)?;
        device.bearer().map_err(|_| RuntimeError::Authorization)?;
        let pin = DevicePin::from(&device);
        let (path, profile_id) = intent_location(console, &device)?;
        let intent = read_intents(&path, &profile_id)?
            .intents
            .into_iter()
            .find(|intent| {
                intent.agent_id == agent
                    && intent.binding_id == ALL_ROOMS
                    && intent.device_id == device.device_id()
            });
        let options_result = self.saved_service_options(console, agent).await;
        let options_warning = options_result
            .as_ref()
            .err()
            .map(|_| "service_preferences_unavailable");
        let options = options_result.ok().flatten();
        let reply = console
            .owner_api_pinned(
                &pin,
                OwnerOperation::Bindings {
                    agent: agent.into(),
                },
            )
            .await
            .map_err(owner_error)?;
        let runtime = self.status(console, agent).await?;
        let services = self.services.lock().await;
        let entry = services.get(&identity(&device, agent));
        let watcher_running =
            entry.is_some_and(|entry| !entry.task.is_finished() && !*entry.cancel.borrow());
        let health = entry
            .map(|entry| entry.health.borrow().clone())
            .unwrap_or_default();
        let running = runtime.as_ref().is_some_and(|status| {
            status.phase != "stopped"
                && status.bindings.iter().any(|child| {
                    matches!(child.phase, "online_chat_only" | "online") || child.phase.starts_with("executing_")
                })
        });
        let native_tools = runtime.as_ref().is_some_and(|status| status.native_tools);
        Ok(serde_json::json!({"runtime":runtime,"service":{
            "enabled":intent.is_some(),"running":running,"watcherRunning":watcher_running,"warning":options_warning,"phase":health.phase,"lastError":health.last_error,"problems":health.problems,"deviceId":device.device_id(),
            "effort":options.as_ref().map(|value| &value.effort).or_else(||intent.as_ref().map(|value| &value.effort)),
            "rooms":reply.value["bindings"]},"nativeTools":native_tools,"strictTokenCap":false}))
    }
    pub(in crate::console) async fn stop_agent_service(
        &self,
        console: &Console,
        agent: &str,
    ) -> Result<Option<&'static str>, RuntimeError> {
        let device = console.authorized_device().await.map_err(device_error)?;
        device.bearer().map_err(|_| RuntimeError::Authorization)?;
        let pin = DevicePin::from(&device);
        let mut services = self.services.lock().await;
        let key = identity(&device, agent);
        if services.get(&key).is_some_and(|entry| entry.pin != pin) {
            return Err(RuntimeError::Authorization);
        }
        let entry = services.remove(&key);
        if let Some(entry) = &entry {
            entry.cancel.send_replace(true);
        }
        let _gate = match &entry {
            Some(entry) => Some(entry.gate.lock().await),
            None => None,
        };
        let (path, profile_id) = intent_location(console, &device)?;
        let preferences = async {
            let options_path = path.with_file_name("agent-service-options.json");
            if options_for_device(
                read_options(&options_path, &profile_id)?,
                agent,
                device.device_id(),
            )
            .is_none()
            {
                let intents = read_intents(&path, &profile_id)?.intents;
                let bindings = console
                    .owner_api_pinned(
                        &pin,
                        OwnerOperation::Bindings {
                            agent: agent.into(),
                        },
                    )
                    .await
                    .ok()
                    .and_then(|reply| {
                        serde_json::from_value::<Vec<Binding>>(reply.value["bindings"].clone()).ok()
                    })
                    .unwrap_or_default();
                let direct = bindings
                    .iter()
                    .find(|binding| {
                        binding.agent_id == agent && binding.scope_kind == "owner_direct"
                    })
                    .map(|binding| binding.id.as_str());
                let candidates: Vec<_> = intents
                    .iter()
                    .filter(|intent| {
                        intent.agent_id == agent && intent.device_id == device.device_id()
                    })
                    .collect();
                let selected = candidates
                    .iter()
                    .copied()
                    .find(|intent| intent.binding_id == ALL_ROOMS)
                    .or_else(|| {
                        candidates
                            .iter()
                            .copied()
                            .find(|intent| Some(intent.binding_id.as_str()) == direct)
                    })
                    .or_else(|| candidates.first().copied());
                if let Some(intent) = selected {
                    save_options(
                        &options_path,
                        &profile_id,
                        ServiceOptions {
                            agent_id: agent.into(),
                            device_id: device.device_id().into(),

                            effort: intent.effort.clone(),
                        },
                    )?;
                }
            }
            Ok::<(), RuntimeError>(())
        }
        .await;
        // Preferences are non-authoritative: corruption or a failed optional
        // write must never prevent Stop from revoking consent/draining workers.
        let consent_result = remove_agent_intents(&path, &profile_id, agent);
        let stop_result = self.cancel_agent_entry(console, &pin, agent).await;
        drop(_gate);
        if let Some(entry) = entry {
            let _ = entry.task.await;
        }
        drop(services);
        consent_result?;
        stop_result?;
        Ok(preferences
            .err()
            .map(|_| "service_preferences_not_preserved"))
    }
    async fn cancel_agent_entry(
        &self,
        console: &Console,
        pin: &DevicePin,
        agent: &str,
    ) -> Result<(), RuntimeError> {
        let device = console.authorized_device().await.map_err(device_error)?;
        if &DevicePin::from(&device) != pin {
            return Err(RuntimeError::Authorization);
        }
        let entries = self.entries.lock().await;
        if entries
            .get(&identity(&device, agent))
            .is_some_and(|entry| &entry.device != pin)
        {
            return Err(RuntimeError::Authorization);
        }
        let key = identity(&device, agent);
        let task = entries.get(&key).map(|entry| {
            entry.cancel.send_replace(true);
            entry.task.clone()
        });
        drop(entries);
        if let Some(task) = task {
            let result = task.join().await;
            let mut entries = self.entries.lock().await;
            if entries
                .get(&key)
                .is_some_and(|entry| entry.task.same(&task))
            {
                entries.remove(&key);
            }
            if result.is_err() {
                return Err(RuntimeError::Stopped);
            }
        }
        Ok(())
    }
    pub(super) async fn drain_services(&self, pin: Option<&DevicePin>) {
        let pending: Vec<_> = self
            .services
            .lock()
            .await
            .iter()
            .filter(|(_, entry)| pin.is_none_or(|pin| pin == &entry.pin))
            .map(|(key, entry)| {
                entry.cancel.send_replace(true);
                (key.clone(), entry.task.clone())
            })
            .collect();
        for (key, task) in pending {
            let _ = task.join().await;
            let mut services = self.services.lock().await;
            if services
                .get(&key)
                .is_some_and(|entry| entry.task.same(&task))
            {
                services.remove(&key);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    const CHILD: &str =
        "console::owned_runtime::service::tests::agent_wide_start_accepts_the_owners_claude_connection_child";
    const CHILD_ROOT: &str = "HAGENCY_SERVICE_CONNECTION_TEST_ROOT";
    // Claude's folder and binary come from the environment. Run in a child
    // test process: the suite's own environment and any installed Claude
    // sign-in stay untouched.
    #[test]
    fn agent_wide_start_accepts_the_owners_claude_connection() {
        let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir(root.join("work")).unwrap();
        std::fs::write(root.join("claude"), b"").unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD, "--ignored", "--nocapture"])
            .env("HOME", &home)
            .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
            .env("HAGENCY_CLAUDE_BINARY", root.join("claude"))
            .env(CHILD_ROOT, &root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
    #[test]
    #[ignore = "isolated child of agent_wide_start_accepts_the_owners_claude_connection"]
    fn agent_wide_start_accepts_the_owners_claude_connection_child() {
        use super::super::super::owner_provider::external;
        const ORIGIN: &str = "https://server.example/";
        const ISSUER: &str = "https://server.example/auth/";
        const OWNER: &str = "@alice:example";
        let root = PathBuf::from(std::env::var_os(CHILD_ROOT).unwrap());
        let state = root.join("state");
        let folder = root.join("home/.claude");
        let config = StartConfig {
            agent_id: "agent-a".into(),
            binding_id: ALL_ROOMS.into(),
            profile: Profile {
                executable: root.join("claude"),
                home: root.join("home"),
                codex_home: folder.clone(),
                cwd: root.join("work"),
                model: "haiku".into(),
                effort: "low".into(),
                shared_auth: true,
                web_search: false,
            },
            credential_ref: runtime::local_reference(runtime::Kind::Claude, &folder, None),
            takeover: Takeover::Never,
            host_files: false,
            shared_workspace: false,
            shared_workspace_pin: None,
            owner_direct: false,
            recovery_device_id: None,
            recovery_consent_id: None,
        };
        let check = |config: &StartConfig| {
            owner_connection(&state, ORIGIN, ISSUER, "subject", OWNER, config)
        };
        // The owner has not chosen "use my local account" yet.
        assert!(matches!(check(&config), Err(RuntimeError::Profile)));
        external::record_connection(&state, ORIGIN, ISSUER, "subject", OWNER, runtime::Kind::Claude)
            .unwrap();
        // This is the start that was refused before.
        check(&config).unwrap();
        // Another owner on this computer has no such connection.
        assert!(matches!(
            owner_connection(&state, ORIGIN, ISSUER, "other", OWNER, &config),
            Err(RuntimeError::Profile)
        ));
        // A profile that names another folder or another binary is not it.
        let mut other = config.clone();
        other.profile.codex_home = root.join("work");
        assert!(matches!(check(&other), Err(RuntimeError::Profile)));
        let mut other = config.clone();
        std::fs::write(root.join("other"), b"").unwrap();
        other.profile.executable = root.join("other");
        assert!(matches!(check(&other), Err(RuntimeError::Profile)));
        // Nor is a Codex reference on Claude's folders.
        let mut other = config.clone();
        other.credential_ref = "codex".into();
        assert!(matches!(check(&other), Err(RuntimeError::Profile)));
    }
    #[test]
    fn obsolete_service_preferences_reset_before_current_options_are_saved() {
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = root.path().join("agent-service-options.json");
        let old = serde_json::json!({"version":2,"profileIdentity":"owner","options":[{
            "agentId":"old","deviceId":"old-device","effort":"low","reservation":10
        }]});
        hagency_store::private::replace(&path, &serde_json::to_vec(&old).unwrap()).unwrap();
        assert!(read_options(&path, "wrong-owner").is_err());
        assert!(read_options(&path, "owner").unwrap().options.is_empty());
        save_options(&path, "owner", ServiceOptions {
            agent_id: "current".into(), device_id: "device".into(), effort: "high".into(),
        }).unwrap();
        let current = read_options(&path, "owner").unwrap();
        assert_eq!(current.version, RUNTIME_STATE_VERSION);
        assert_eq!(current.options.len(), 1);
        assert_eq!(current.options[0].agent_id, "current");
        assert!(!std::fs::read_to_string(path).unwrap().contains("reservation"));
    }
    #[test]
    fn preferences_are_owner_and_device_scoped_without_execution_consent() {
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = root.path().join("agent-service-options.json");
        save_options(
            &path,
            "owner-a",
            ServiceOptions {
                agent_id: "a".into(),
                device_id: "here".into(),

                effort: "high".into(),
            },
        )
        .unwrap();
        assert!(read_options(&path, "owner-b").is_err());
        assert!(
            options_for_device(read_options(&path, "owner-a").unwrap(), "a", "there").is_none()
        );
        assert_eq!(
            options_for_device(read_options(&path, "owner-a").unwrap(), "a", "here")
                .unwrap()
                .effort,
            "high"
        );
        assert!(
            read_intents(&path.with_file_name("runtime-intents.json"), "owner-a")
                .unwrap()
                .intents
                .is_empty()
        );
    }
    #[test]
    fn transient_worker_retries_back_off_without_becoming_permanent() {
        let at = Instant::now();
        let mut retries = BTreeMap::new();
        for _ in 0..20 {
            schedule_retry(&mut retries, "room", "agent_lease_busy", at);
        }
        let retry = retries.get("room").unwrap();
        assert_eq!(retry.next.duration_since(at), Duration::from_secs(60));
        assert_eq!(retry.error, "agent_lease_busy");
        assert!(retry.next <= at + Duration::from_secs(60));
    }
    #[test]
    fn discovery_excludes_paused_and_foreign_rooms_and_discovers_new_rooms() {
        let binding = |id: &str, agent: &str, state: &str| Binding {
            id: id.into(),
            agent_id: agent.into(),
            room_id: format!("!{id}:example.test"),
            state: state.into(),
            scope_kind: String::new(),
        };
        let mut rooms = vec![
            binding("private", "a", "active"),
            binding("paused", "a", "suspended"),
            binding("foreign", "b", "active"),
        ];
        assert_eq!(
            active_binding_ids(&rooms, "a"),
            BTreeSet::from(["private".into()])
        );
        rooms.push(binding("new", "a", "active"));
        assert_eq!(
            active_binding_ids(&rooms, "a"),
            BTreeSet::from(["private".into(), "new".into()])
        );
    }
    #[test]
    fn stop_removes_all_room_recovery_and_invalidates_watcher_consent() {
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = root.path().join("runtime-intents.json");
        let intended = |agent: &str, binding: &str| RuntimeIntent {
            agent_id: agent.into(),
            binding_id: binding.into(),
            device_id: "device".into(),
            consent_id: "a".repeat(64),

            effort: "low".into(),
            host_files: false,
            shared_workspace: false,
            shared_workspace_pin: None,
        };
        let global = intended("a", ALL_ROOMS);
        for intent in [
            global.clone(),
            intended("a", "room"),
            intended("b", "other"),
        ] {
            change_intent(
                &path,
                "owner",
                &intent.agent_id,
                &intent.binding_id,
                Some(intent.clone()),
            )
            .unwrap();
        }
        assert!(service_enabled(&path, "owner", &global).unwrap());
        remove_agent_intents(&path, "owner", "a").unwrap();
        assert!(!service_enabled(&path, "owner", &global).unwrap());
        assert_eq!(read_intents(&path, "owner").unwrap().intents.len(), 1);
        assert!(
            change_intent_checked(
                &path,
                "owner",
                "a",
                ALL_ROOMS,
                Some(global.clone()),
                Some(&global.consent_id)
            )
            .is_err()
        );
    }
    #[test]
    fn global_recovery_supersedes_room_snapshots_only_on_its_device() {
        let intended = |binding: &str, device: &str| RuntimeIntent {
            agent_id: "a".into(),
            binding_id: binding.into(),
            device_id: device.into(),
            consent_id: "a".repeat(64),

            effort: "low".into(),
            host_files: false,
            shared_workspace: false,
            shared_workspace_pin: None,
        };
        let values = RuntimeIntents {
            version: RUNTIME_STATE_VERSION,
            profile_identity: "owner".into(),
            intents: vec![
                intended(ALL_ROOMS, "here"),
                intended("room", "here"),
                intended("other", "there"),
            ],
        };
        let found = intents_for_device(values, "here");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].binding_id, ALL_ROOMS);
    }
}

#[cfg(test)]
mod configuration_busy_tests {
    use super::*;
    #[tokio::test]
    async fn model_conflict_is_agent_scoped_while_provider_conflict_is_owner_scoped() {
        let (device, _) = super::super::super::server_login::fixture_device(
            "http://127.0.0.1/",
            "subject",
            "@owner:test",
        );
        let manager = OwnedRuntime::default();
        let (cancel, mut receiver) = watch::channel(false);
        let task = tokio::spawn(async move {
            let _ = receiver.changed().await;
        });
        let (_sender, health) = watch::channel(ServiceHealth::default());
        manager.services.lock().await.insert(
            identity(&device, "agent_y"),
            ServiceEntry {
                pin: DevicePin::from(&device),
                gate: Arc::new(Mutex::new(())),
                cancel,
                task: manager.tasks.track(task),
                health,
            },
        );
        assert!(
            !manager.agent_busy(&device, "agent_x").await,
            "an active sibling Agent must not block this Agent's saved model"
        );
        assert!(
            manager.agent_busy(&device, "agent_y").await,
            "target Agent's captured profile must be fenced"
        );
        assert!(
            manager.profile_busy(&device).await,
            "owner provider association remains blocked by any active Agent"
        );
        manager.stop_all().await;
        assert!(!manager.agent_busy(&device, "agent_y").await);
        assert!(!manager.profile_busy(&device).await);
    }
}
