//! Closed project-connection frontend adapter. Private JSON only enters through
//! a native document picker; script requests contain public metadata alone.
use super::{Host, Lease, ManagementPermission, NativeManagement, Value, parse, value};
use ::hagency::native_owner::{
    WebConnectionEdit, WebConnectionProbeInput, NativeConnectionRegistration,
};
use ::hagency::daemon::bridge::WebConnectionReview;
use serde::Deserialize;
use std::time::{Duration, Instant};

pub(crate) const LIST: &str = "hagency.web_connections.list";
pub(crate) const GET: &str = "hagency.web_connections.get";
pub(crate) const PROBE: &str = "hagency.web_connections.probe";
pub(crate) const RESULT: &str = "hagency.web_connections.command.get";
pub(crate) const VERIFY: &str = "hagency.web_connections.verify";
pub(crate) const CREATE: &str = "hagency.web_connections.create";
pub(crate) const ALLOWANCE: &str = "hagency.web_connections.allowance";
pub(crate) const WITHDRAW: &str = "hagency.web_connections.withdraw";
pub(crate) const ISSUE: &str = "hagency.web_connections.issue";
pub(crate) const IMPORT: &str = "hagency.web_connections.import";
pub(crate) const REPLACE: &str = "hagency.web_connections.replace";
pub(crate) const DOWNLOAD: &str = "hagency.web_connections.download";
pub(crate) fn is_consent(service: &str) -> bool {
    matches!(
        service,
        CREATE | ALLOWANCE | WITHDRAW | ISSUE | IMPORT | REPLACE | DOWNLOAD
    )
}
pub(super) fn permissions(service: &str) -> &'static [ManagementPermission] {
    use ManagementPermission::*;
    match service {
        LIST | GET | PROBE => &[ReadConnections],
        RESULT | VERIFY => &[ReadConnections, ContributeResources],
        v if is_consent(v) => &[ReadConnections, ContributeResources],
        _ => &[],
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    after: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Detail {
    id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Saved {
    command_id: String,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Identity {
    command_id: String,
    id: String,
    expected_revision: u64,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Import {
    command_id: String,
    id: String,
    expected_revision: u64,
    label: String,
    homeserver: String,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Download {
    id: String,
    expected_revision: u64,
}
fn invalid() -> String {
    "invalid_management_arguments".into()
}
fn identifier(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || matches!(v, b'_' | b'-'))
    {
        Err(invalid())
    } else {
        Ok(())
    }
}
fn server(id: &str) -> Result<(), String> {
    WebConnectionProbeInput {
        server_name: Some(id.into()),
        url: None,
    }
    .validate()
    .map_err(|e| e.code)
}
fn revision(value: u64, zero: bool) -> Result<(), String> {
    if value > 9_007_199_254_740_991 || (!zero && value == 0) {
        Err(invalid())
    } else {
        Ok(())
    }
}
fn identity(v: &Identity) -> Result<(), String> {
    identifier(&v.command_id)?;
    server(&v.id)?;
    revision(v.expected_revision, false)
}
pub(super) enum Call {
    List(Option<String>),
    Get(String),
    Probe(WebConnectionProbeInput),
    Result(String),
    Verify(String, String, u64),
}
pub(super) fn call(service: &str, args: Value) -> Result<Option<Call>, String> {
    Ok(Some(match service {
        LIST => {
            let q: Page = parse(args)?;
            if let Some(id) = &q.after {
                server(id)?
            }
            Call::List(q.after)
        }
        GET => {
            let q: Detail = parse(args)?;
            server(&q.id)?;
            Call::Get(q.id)
        }
        PROBE => {
            let q: WebConnectionProbeInput = parse(args)?;
            q.validate().map_err(|e| e.code)?;
            Call::Probe(q)
        }
        RESULT => {
            let q: Saved = parse(args)?;
            identifier(&q.command_id)?;
            Call::Result(q.command_id)
        }
        VERIFY => {
            let q: Identity = parse(args)?;
            identity(&q)?;
            Call::Verify(q.command_id, q.id, q.expected_revision)
        }
        v if is_consent(v) => return Err("web_connection_review_required".into()),
        _ => return Ok(None),
    }))
}
impl Call {
    pub(super) fn mutation(&self) -> bool {
        matches!(self, Self::Verify(..))
    }
    pub(super) async fn execute(self, manager: &NativeManagement) -> Result<Value, String> {
        match self {
            Self::List(after) => value(manager.web_connections(after).await.map_err(|e| e.code)?),
            Self::Get(id) => value(manager.web_connection(id).await.map_err(|e| e.code)?),
            Self::Probe(input) => value(
                manager
                    .probe_web_connection(input)
                    .await
                    .map_err(|e| e.code)?,
            ),
            Self::Result(id) => value(
                manager
                    .web_connection_result(id)
                    .await
                    .map_err(|e| e.code)?,
            ),
            Self::Verify(command, id, revision) => value(
                manager
                    .verify_web_connection(command, id, revision)
                    .await
                    .map_err(|e| e.code)?,
            ),
        }
    }
}
enum Input {
    Edit(WebConnectionEdit),
    Import(Import),
    Replace(Identity),
    Download(Download),
}
fn input(service: &str, mut args: Value) -> Result<Input, String> {
    Ok(match service {
        CREATE | ALLOWANCE | WITHDRAW | ISSUE => {
            let object = args.as_object_mut().ok_or_else(invalid)?;
            if object.contains_key("operation") {
                return Err(invalid());
            }
            object.insert(
                "operation".into(),
                Value::String(
                    match service {
                        CREATE => "create",
                        ALLOWANCE => "allowance",
                        WITHDRAW => "withdraw",
                        _ => "issue",
                    }
                    .into(),
                ),
            );
            let edit: WebConnectionEdit = parse(args)?;
            edit.validate().map_err(|e| e.code)?;
            Input::Edit(edit)
        }
        IMPORT => {
            let v: Import = parse(args)?;
            identifier(&v.command_id)?;
            server(&v.id)?;
            revision(v.expected_revision, true)?;
            WebConnectionProbeInput {
                server_name: None,
                url: Some(v.homeserver.clone()),
            }
            .validate()
            .map_err(|e| e.code)?;
            if v.label.len() > 256
                || v.label.trim() != v.label
                || v.label.chars().any(char::is_control)
            {
                return Err(invalid());
            }
            Input::Import(v)
        }
        REPLACE => {
            let v: Identity = parse(args)?;
            identity(&v)?;
            Input::Replace(v)
        }
        DOWNLOAD => {
            let v: Download = parse(args)?;
            server(&v.id)?;
            revision(v.expected_revision, false)?;
            Input::Download(v)
        }
        _ => return Err("unknown_hagency_service".into()),
    })
}
enum Loaded {
    Review(WebConnectionReview),
    Download(NativeConnectionRegistration),
}
pub(crate) struct ConnectionConsent {
    host: Host,
    lease: Lease,
    service: String,
    input: Input,
    expires: Instant,
    loaded: Option<Loaded>,
}
pub(crate) enum ConnectionOutcome {
    Applied(Value),
    Export {
        filename: String,
        bytes: Vec<u8>,
        lease: Lease,
        account: String,
        id: String,
    },
}
impl Host {
    pub fn web_connection_consent(
        &self,
        lease: Lease,
        service: &str,
        args: Value,
    ) -> Result<ConnectionConsent, String> {
        let input = input(service, args)?;
        let account = self.check(&lease)?;
        lease.authorize(&account, service, None)?;
        Ok(ConnectionConsent {
            host: self.clone(),
            lease,
            service: service.into(),
            input,
            expires: Instant::now() + Duration::from_secs(180),
            loaded: None,
        })
    }
}
impl ConnectionConsent {
    pub fn account(&self) -> &str {
        &self.lease.identity().account
    }
    pub fn expired(&self) -> bool {
        Instant::now() >= self.expires
    }
    pub fn ready(&self) -> bool {
        self.loaded.is_some() && !self.expired()
    }
    pub fn needs_file(&self) -> bool {
        matches!(self.input, Input::Import(_) | Input::Replace(_))
    }
    pub fn picker_identity(&self) -> (Lease, String) {
        (self.lease.clone(), self.account().into())
    }
    fn authorize(&self) -> Result<(), String> {
        if self.expired() {
            return Err("provider_confirmation_expired".into());
        }
        let account = self.host.check(&self.lease)?;
        self.lease.authorize(&account, &self.service, None)
    }
    pub async fn load(mut self, private_file: Option<String>) -> Result<Self, String> {
        self.authorize()?;
        let manager = self.host.manager(&self.lease).await?;
        self.loaded = Some(match &self.input {
            Input::Edit(input) => Loaded::Review(
                manager
                    .review_web_connection(input.clone())
                    .await
                    .map_err(|e| e.code)?,
            ),
            Input::Import(input) => {
                let raw = private_file.as_deref().ok_or("connection_file_required")?;
                let review = manager
                    .review_web_connection_import(
                        input.command_id.clone(),
                        input.expected_revision,
                        input.label.clone(),
                        input.homeserver.clone(),
                        raw,
                    )
                    .await
                    .map_err(|e| e.code)?;
                if review.summary.id != input.id {
                    return Err("connection_import_server_mismatch".into());
                }
                Loaded::Review(review)
            }
            Input::Replace(input) => {
                let raw = private_file.as_deref().ok_or("connection_file_required")?;
                if raw.len() > 16 * 1024 {
                    return Err(invalid());
                }
                let credential: Value = serde_json::from_str(raw).map_err(|_| invalid())?;
                if !credential.as_object().is_some_and(|v| {
                    v.keys().all(|k| {
                        [
                            "kind",
                            "asToken",
                            "hsToken",
                            "url",
                            "namespace",
                            "senderLocalpart",
                            "registrationToken",
                            "representativeToken",
                        ]
                        .contains(&k.as_str())
                    })
                }) {
                    return Err(invalid());
                }
                Loaded::Review(
                    manager
                        .review_web_connection_credential(
                            input.command_id.clone(),
                            input.id.clone(),
                            input.expected_revision,
                            credential,
                        )
                        .await
                        .map_err(|e| e.code)?,
                )
            }
            Input::Download(input) => Loaded::Download(
                manager
                    .download_web_connection_registration(input.id.clone(), input.expected_revision)
                    .await
                    .map_err(|e| e.code)?,
            ),
        });
        self.authorize()?;
        Ok(self)
    }
    pub fn details(&self) -> String {
        match &self.loaded {
            Some(Loaded::Review(v)) => {
                let s = &v.summary;
                let title = match s.operation.as_str() {
                    "create" => "Create external connection",
                    "allowance" => "Change declared connection allowance",
                    "withdraw" => "Withdraw stored connection credentials",
                    "issue" => "Issue private appservice registration",
                    "replace" => "Replace stored connection credentials",
                    "import" => "Import approved Palpo configuration",
                    _ => "Review connection",
                };
                let mut rows = vec![
                    crate::i18n::tr(title).into(),
                    s.id.clone(),
                    format!("{}: {}", crate::i18n::tr("Revision"), s.expected_revision),
                ];
                if let Some(v) = &s.api_base_url {
                    rows.push(v.clone())
                } else if let Some(v) = &s.current {
                    rows.push(v.api_base_url.clone())
                }
                if let Some(v) = &s.registration_url {
                    rows.push(v.clone())
                }
                if s.operation == "allowance" {
                    rows.push(format!(
                        "{}: {}",
                        crate::i18n::tr("Token allowance"),
                        s.allocated_tokens.map_or_else(
                            || crate::i18n::tr("Unallocated").into(),
                            |v| v.to_string()
                        )
                    ))
                }
                if s.original_result.is_some() {
                    rows.push(
                        crate::i18n::tr(
                            "Recover the original saved result; do not repeat the change.",
                        )
                        .into(),
                    )
                }
                rows.join("\n")
            }
            Some(Loaded::Download(v)) => format!(
                "{}\n{}",
                crate::i18n::tr("Save private registration file"),
                v.filename
            ),
            None => crate::i18n::tr("Reading connection configuration…").into(),
        }
    }
    pub fn words(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        if matches!(self.input, Input::Download(_)) {
            (
                "Save private registration file",
                "Choose where to save this registration using the system dialog.",
                "This file contains credentials. It is not sent to the mini app or a chat room.",
                "Choose destination",
            )
        } else if matches!(
            &self.input,
            Input::Edit(WebConnectionEdit::Allowance { .. })
        ) {
            (
                "Review legacy fleet allowance",
                "This preserves the legacy fleet allowance in this connection configuration.",
                "Desktop does not enforce this legacy allowance. Current resource-pool allocations and approved grants are unchanged.",
                "Save configuration",
            )
        } else {
            (
                "Review project connection",
                "Check the exact server and changes before saving.",
                "This saves local connection configuration. Server admission, capacity grants and agent execution remain separate.",
                "Save configuration",
            )
        }
    }
    pub async fn accept(mut self) -> Result<ConnectionOutcome, String> {
        self.authorize()?;
        let manager = self.host.manager(&self.lease).await?;
        let loaded = self.loaded.take().ok_or("web_connection_review_required")?;
        match loaded {
            Loaded::Review(review) => {
                let result = manager
                    .apply_web_connection(review, self.expires)
                    .await
                    .map_err(|e| e.code);
                self.authorize()
                    .map_err(|_| "management_outcome_unknown".to_owned())?;
                Ok(ConnectionOutcome::Applied(value(result?)?))
            }
            Loaded::Download(_) => {
                let Input::Download(input) = &self.input else {
                    return Err(invalid());
                };
                // Re-read exact revision after confirmation, before export.
                let file = manager
                    .download_web_connection_registration(input.id.clone(), input.expected_revision)
                    .await
                    .map_err(|e| e.code)?;
                self.authorize()?;
                Ok(ConnectionOutcome::Export {
                    filename: file.filename,
                    bytes: file.bytes,
                    lease: self.lease.clone(),
                    account: self.account().into(),
                    id: input.id.clone(),
                })
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn connection_dispatch_never_accepts_script_credentials_or_paths() {
        let base = json!({"commandId":"original","id":"matrix.test","expectedRevision":1});
        assert!(input(REPLACE, base.clone()).is_ok());
        for key in [
            "credential",
            "path",
            "configuration",
            "token",
            "owner",
            "profile",
            "transport",
        ] {
            let mut v = base.clone();
            v[key] = json!("secret");
            assert!(input(REPLACE, v).is_err())
        }
        assert!(call(REPLACE, base).is_err());
        assert!(input(CREATE,json!({"commandId":"create","id":"matrix.test","label":"Matrix","apiBaseUrl":"https://matrix.test","operation":"withdraw"})).is_err());
        assert!(call(VERIFY,json!({"commandId":"verify","id":"matrix.test","expectedRevision":1,"state":"accepted"})).is_err());
    }
}
