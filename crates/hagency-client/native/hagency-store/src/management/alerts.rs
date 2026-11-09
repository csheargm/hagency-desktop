//! Resource-scoped retained alerts. Triage is display state, never admission.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub struct ResourceAlertQuery {
    pub resource_id: String,
    #[serde(default)] pub after: String,
    #[serde(default)] pub status: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub struct ResourceAlertDetailQuery {
    pub resource_id: String,
    pub id: String,
    #[serde(default)] pub after_note: u64,
}
pub(crate) fn alert_key(value: &str) -> Result<(), Error> {
    if value.is_empty() || value.len()>256 || !value.bytes().all(|b| b.is_ascii_alphanumeric() || b"_:-.".contains(&b)) {
        return Err(hagency_core::InvalidInput("invalid alert key").into());
    }
    Ok(())
}
impl ResourceAlertQuery {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.resource_id,128)?;
        if !self.after.is_empty() { alert_key(&self.after)?; }
        if self.status.as_deref().is_some_and(|s| !crate::ALERT_STATUSES.contains(&s)) { return Err(hagency_core::InvalidInput("invalid alert status").into()); }
        Ok(())
    }
}
impl ResourceAlertDetailQuery {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.resource_id,128)?;alert_key(&self.id)?;
        if self.after_note>hagency_core::JSON_SAFE_MAX {return Err(Error::Capacity);}
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub struct TriageResourceAlert {
    pub command_id: String,
    pub resource_id: String,
    pub id: String,
    pub expected_revision: String,
    pub to: String,
    pub note: Option<String>,
}
impl TriageResourceAlert {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.command_id,128)?;
        ResourceAlertDetailQuery{resource_id:self.resource_id.clone(),id:self.id.clone(),after_note:0}.validate()?;
        if self.expected_revision.len()!=64 || !self.expected_revision.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            || !crate::ALERT_STATUSES.contains(&self.to.as_str())
            || self.note.as_ref().is_some_and(|n| n.len()>2048 || n.trim()!=n || n.chars().any(|c| c.is_control() && c!='\n')) {
            return Err(hagency_core::InvalidInput("invalid alert triage").into());
        }
        Ok(())
    }
}
/// Public numeric evidence only; arbitrary provider detail JSON never crosses
/// the mini-app boundary. Operator-authored summary/runbook/notes remain text.
///
/// `kind` and `params` identify the alert for localized display. `kind` is one
/// of [`crate::ALERT_KINDS`] and `params` holds exactly that kind's documented
/// keys (see `crate::domain::ceiling_alerts::management`). The English
/// `summary`/`impact`/`runbook`/`recoveryCondition` stay for older clients.
/// Both default when an older triage receipt is replayed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all="camelCase")]
pub struct ResourceAlert {
    pub id:String, pub resource_id:String, pub revision:String,
    pub summary:String, pub runbook:String, pub impact:String, pub recovery_condition:String,
    pub status:String, pub severity:String, pub occurrences:u64,
    pub first_seen_ms:u64, pub last_seen_ms:u64, pub resolved:bool,
    pub note:Option<String>, pub assignee:Option<String>, pub suppress_until_ms:Option<u64>,
    pub next:Vec<String>, pub budget:std::collections::BTreeMap<String,u64>,
    #[serde(default="other_alert_kind")] pub kind:String,
    #[serde(default)] pub params:serde_json::Map<String,serde_json::Value>,
}
fn other_alert_kind()->String {crate::ALERT_KIND_OTHER.into()}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ResourceAlertPage {
    pub resource_id:String, pub alerts:Vec<ResourceAlert>, pub next_after:Option<String>,
    pub observed_at_ms:u64,
}
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all="camelCase")]
pub struct ResourceAlertNote { pub sequence:u64, pub author:String, pub text:String, pub at_ms:u64 }
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all="camelCase")]
pub struct ResourceAlertDetail {
    pub alert:ResourceAlert, pub notes:Vec<ResourceAlertNote>, pub next_note:Option<u64>,
}
impl Session {
    /// Native diagnostic producer only. This permission is not exposed by the
    /// mini-app host. It neither inherits triage authority nor returns raw data.
    pub async fn observe_resource_alert(&self, context: &HostContext, resource_id: String, now: u64) -> Result<crate::SweepOutcome, Error> {
        identifier(&resource_id, 128)?;
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        self.check(context, Permission::ObserveResourceAlerts)?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => store.observe_resource_alert(&self.current_profile(store)?, resource_id, now).await?,
        };
        self.check(context, Permission::ObserveResourceAlerts).map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }

    pub async fn resource_alerts(&self, context:&HostContext, query:ResourceAlertQuery) -> Result<ResourceAlertPage,Error> {
        query.validate()?;
        let _pending=self.scope.pending.try_acquire().map_err(|_|Error::Busy)?;
        self.check(context,Permission::ReadAlerts)?;
        let result=match self.local_store()? {
            SessionStore::Current(store)=>store.resource_alerts(&self.current_profile(store)?,query).await?,
        };
        self.check(context,Permission::ReadAlerts)?;Ok(result)
    }
    pub async fn resource_alert(&self, context:&HostContext, query:ResourceAlertDetailQuery) -> Result<ResourceAlertDetail,Error> {
        query.validate()?;
        let _pending=self.scope.pending.try_acquire().map_err(|_|Error::Busy)?;
        self.check(context,Permission::ReadAlerts)?;
        let result=match self.local_store()? {
            SessionStore::Current(store)=>store.resource_alert(&self.current_profile(store)?,query).await?,
        };
        self.check(context,Permission::ReadAlerts)?;Ok(result)
    }
    pub async fn triage_resource_alert(&self, context:&HostContext, input:TriageResourceAlert) -> Result<ResourceAlert,Error> {
        input.validate()?;
        let _pending=self.scope.pending.try_acquire().map_err(|_|Error::Busy)?;
        let _command=self.scope.commands.lock().await;
        self.check(context,Permission::TriageAlerts)?;
        let profile=match self.local_store()? { SessionStore::Current(store)=>store.identity().0};
        let receipt=CommandReceipt::new(context,"alert.triage",&input.command_id,&serde_json::json!({"profile":profile,"input":input}))?;
        let actor=context.account.clone();
        let result=match self.local_store()? {
            SessionStore::Current(store)=>store.triage_resource_alert(&self.current_profile(store)?,input,actor,receipt).await?,
        };
        self.check(context,Permission::TriageAlerts).map_err(|_|Error::OutcomeUnknown)?;Ok(result)
    }
}
fn require_alert_resource(db:&Connection,id:&str)->Result<(),Error>{
    if db.query_row("SELECT 1 FROM resources WHERE id=?1",[id],|r|r.get::<_,u8>(0)).optional()?.is_none(){return Err(Error::NotFound);}
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn triage_receipts_recorded_before_alert_kinds_still_replay() {
        // The exact shape an original `alert.triage` receipt stored before
        // `kind`/`params` existed. Replay must not fail on the missing keys.
        let old = serde_json::json!({"id":"agent_ceiling_overrun:r","resourceId":"r","revision":"a".repeat(64),
            "summary":"s","runbook":"r","impact":"i","recoveryCondition":"c","status":"acknowledged",
            "severity":"warning","occurrences":1,"firstSeenMs":1,"lastSeenMs":2,"resolved":false,"note":null,
            "assignee":null,"suppressUntilMs":null,"next":["assigned","resolved"],"budget":{"ceilingTokens":1}});
        let replayed: ResourceAlert = serde_json::from_value(old).unwrap();
        assert_eq!(replayed.kind, crate::ALERT_KIND_OTHER);
        assert!(replayed.params.is_empty());
        let current = serde_json::to_value(&replayed).unwrap();
        assert_eq!(current["kind"], "other");
        assert_eq!(current["params"], serde_json::json!({}));
    }
}
