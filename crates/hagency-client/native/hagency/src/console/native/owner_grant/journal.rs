use std::path::{Path, PathBuf};
const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_RECORDS: usize = 128;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Identity {
    origin: String,
    owner: String,
    issuer: String,
    subject: String,
    client: String,
    installation: String,
    device: String,
}
impl From<super::project_setup::Identity> for Identity {
    fn from(value: super::project_setup::Identity) -> Self {
        Self {
            origin: value.origin,
            owner: value.owner,
            issuer: value.issuer,
            subject: value.subject,
            client: value.client,
            installation: value.installation,
            device: value.device,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum State {
    Review,
    Applying,
    Recorded,
    Declined,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    original: Original,
    original_digest: String,
    state: State,
    // Retained immutable original bytes, never a current authorization proof.
    receipt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    publication: Option<PublicationIntent>,
}
impl Record {
    fn new(original: Original) -> Result<Self, NativeError> {
        let original_digest = digest(&original)?;
        Ok(Self {
            original,
            original_digest,
            state: State::Review,
            receipt: None,
            publication: None,
        })
    }
    fn progress(&self, state: OwnerGrantState) -> OwnerGrantProgress {
        OwnerGrantProgress {
            summary: self.summary(),
            state,
            closure: None,
            succession_state: None,
        }
    }
    /// The original summary; a recorded successor shows what its receipt
    /// carried rather than what the form estimated.
    fn summary(&self) -> OwnerGrantSummary {
        let mut summary = self.original.summary();
        if let (Some(view), Some(bytes)) = (summary.succession.as_mut(), &self.receipt)
            && let Ok(receipt) = serde_json::from_str::<NativeGrantReceipt>(bytes)
            && let Some(link) = receipt.succession
        {
            view.carried_children = link.carried_children;
            view.carried_retained_tokens = link.carried_retained_tokens;
        }
        summary
    }
    fn record(&mut self, receipt: &NativeGrantReceipt) -> Result<(), NativeError> {
        if !self.original.matches_receipt(receipt) {
            return Err(history_error());
        }
        let bytes = serde_json::to_string(receipt).map_err(|_| history_error())?;
        if self.receipt.as_ref().is_some_and(|old| old != &bytes) {
            return Err(history_error());
        }
        self.receipt.get_or_insert(bytes);
        self.state = State::Recorded;
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Journal {
    version: u32,
    identity: Identity,
    records: Vec<Record>,
}
struct History {
    path: PathBuf,
    data: Journal,
}
impl History {
    fn marker(path: &Path) -> PathBuf {
        path.with_file_name("owner-grant-intents-v1.identity")
    }
    fn load(path: &Path, identity: Identity) -> Result<Self, NativeError> {
        let marker = Self::marker(path);
        match (path.exists(), marker.exists()) {
            (true, true) => {
                let bytes = crate::console::server_login::read_private_json(&marker, 8192)
                    .map_err(|_| history_error())?;
                if serde_json::from_slice::<Identity>(&bytes).map_err(|_| history_error())?
                    != identity
                {
                    return Err(history_error());
                }
            }
            (false, false) => {}
            _ => return Err(history_error()),
        }
        let data = if path.exists() {
            let bytes = crate::console::server_login::read_private_json(path, MAX_BYTES as u64)
                .map_err(|_| history_error())?;
            serde_json::from_slice::<Journal>(&bytes).map_err(|_| history_error())?
        } else {
            Journal {
                version: 1,
                identity: identity.clone(),
                records: vec![],
            }
        };
        if data.version != 1 || data.identity != identity || data.records.len() > MAX_RECORDS {
            return Err(history_error());
        }
        let mut ids = std::collections::BTreeSet::new();
        for record in &data.records {
            record.original.validate(&identity)?;
            if record.original_digest != digest(&record.original)?
                || !ids.insert(&record.original.intent_id)
                || !ids.insert(&record.original.selection.command_id)
                || !ids.insert(&record.original.selection.grant_id)
                || (record.state == State::Recorded) != record.receipt.is_some()
            {
                return Err(history_error());
            }
            if let Some(publication) = &record.publication {
                super::key(&publication.command_id)?;
                positive(publication.expected_contribution_revision)?;
                if let Some(revision) = publication.expected_predecessor_revision {
                    positive(revision)?;
                }
                if record.state != State::Recorded
                    || !ids.insert(&publication.command_id)
                    || record.original.succession.is_some()
                        != publication.expected_predecessor_revision.is_some()
                {
                    return Err(history_error());
                }
            }
            if let Some(bytes) = &record.receipt {
                if bytes.len() > 12 * 1024 {
                    return Err(history_error());
                }
                let receipt: NativeGrantReceipt =
                    serde_json::from_str(bytes).map_err(|_| history_error())?;
                if !record.original.matches_receipt(&receipt) {
                    return Err(history_error());
                }
            }
        }
        Ok(Self {
            path: path.into(),
            data,
        })
    }
    fn save(&self) -> Result<(), NativeError> {
        let bytes = serde_json::to_vec(&self.data).map_err(|_| history_error())?;
        if bytes.len() > MAX_BYTES {
            return Err(failure(409, "owner_grant_history_full"));
        }
        let marker = Self::marker(&self.path);
        if !marker.exists() {
            if self.path.exists() {
                return Err(history_error());
            }
            let identity = serde_json::to_vec(&self.data.identity).map_err(|_| history_error())?;
            hagency_store::private::replace(&marker, &identity).map_err(|_| history_error())?;
        }
        hagency_store::private::replace(&self.path, &bytes).map_err(|_| history_error())
    }
    fn index(&self, intent: &str) -> Result<usize, NativeError> {
        self.data
            .records
            .iter()
            .position(|r| r.original.intent_id == intent)
            .ok_or_else(|| failure(404, "owner_grant_intent_missing"))
    }
    /// `prior` names the canonical grants that a new v2 original may follow:
    /// closed grants of another admission and, for a `regrant`, grants of this
    /// same admission whose UTC month ended before the new one starts.
    fn insert_after(
        &mut self,
        original: Original,
        prior: &[String],
        regrant: bool,
        void: &[String],
    ) -> Result<(), NativeError> {
        original.validate(&self.data.identity)?;
        if self.data.records.len() >= MAX_RECORDS {
            return Err(failure(409, "owner_grant_history_full"));
        }
        if self.data.records.iter().any(|r| {
            r.state != State::Declined
                && !void.contains(&r.original.selection.grant_id)
                && r.original.binding.contribution_id == original.binding.contribution_id
                && !(original.receipt_version >= 2
                    && r.state == State::Recorded
                    && prior.contains(&r.original.selection.grant_id)
                    && (r.original.admission.as_ref().map(|a| &a.request_id)
                        != original.admission.as_ref().map(|a| &a.request_id)
                        || (regrant && r.original.period.end_ms <= original.period.start_ms)))
        }) {
            return Err(failure(409, "owner_grant_original_intent_required"));
        }
        self.data.records.push(Record::new(original)?);
        self.save()
    }
    /// Journal a successor of the contribution's recorded chain head. Every
    /// other original of the contribution must be resolved: declined or
    /// recorded. The native writer repeats the chain-head check.
    fn insert_successor(&mut self, original: Original) -> Result<(), NativeError> {
        original.validate(&self.data.identity)?;
        let link = original.succession.as_ref().ok_or_else(history_error)?;
        if self.data.records.len() >= MAX_RECORDS {
            return Err(failure(409, "owner_grant_history_full"));
        }
        let contribution = &original.binding.contribution_id;
        let predecessor = self
            .data
            .records
            .iter()
            .find(|r| r.original.intent_id == link.predecessor_intent_id)
            .ok_or_else(history_error)?;
        if predecessor.state != State::Recorded
            || predecessor.original.selection.grant_id != link.predecessor_grant_id
            || &predecessor.original.binding.contribution_id != contribution
            || !admission::AdmissionRound::same_request(
                &predecessor.original.admission,
                &original.admission,
            )
        {
            return Err(history_error());
        }
        if self.data.records.iter().any(|r| {
            &r.original.binding.contribution_id == contribution
                && matches!(r.state, State::Review | State::Applying)
        }) {
            return Err(failure(409, "owner_grant_succession_pending"));
        }
        self.data.records.push(Record::new(original)?);
        self.save()
    }
    /// The contribution has recorded grants on this admission and every one
    /// covers a UTC month that ended by `now`: a new month may be granted on
    /// the same admission. The canonical writer repeats this check.
    fn admission_month_ended(
        &self,
        contribution: &str,
        admission: &str,
        now: u64,
        void: &[String],
    ) -> bool {
        let mut recorded = self
            .data
            .records
            .iter()
            .filter(|r| {
                r.state == State::Recorded
                    && !void.contains(&r.original.selection.grant_id)
                    && r.original.binding.contribution_id == contribution
                    && r.original
                        .admission
                        .as_ref()
                        .is_some_and(|a| a.request_id == admission)
            })
            .peekable();
        recorded.peek().is_some() && recorded.all(|r| r.original.period.end_ms <= now)
    }
}
