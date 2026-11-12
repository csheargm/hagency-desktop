//! Profile-scoped side configuration. Never writes legacy side_records or starts
//! a fleet/agent. The shared metadata table is filtered by this private kind.
use super::*;
use crate::management::{
    CommandReceipt, WebConnection, WebConnectionChange, WebConnectionCommand, WebConnectionPage,
    WebConnectionPrivate, WebConnectionResult, web_connection_credential_digest,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const KIND: &str = "connection_private_v1";
const MAX_RECORD: usize = 128 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    public: WebConnection,
    live: Option<crate::Credential>,
    pending: Option<crate::Credential>,
    registration: Option<String>,
    transport: Option<Value>,
}
#[derive(Serialize, Deserialize)]
struct Receipt {
    profile: String,
    result: WebConnectionResult,
}
fn read(db: &Connection, profile: &str, id: &str) -> Result<Option<Record>, Error> {
    let raw:Option<String>=db.query_row("SELECT value FROM owner_web_catalog_settings WHERE profile=?1 AND kind=?2 AND entity=?3",params![profile,KIND,id],|r|r.get(0)).optional()?;
    raw.map(|s| {
        if s.len() > MAX_RECORD {
            return Err(Error::Capacity);
        }
        let row: Record = serde_json::from_str(&s)?;
        if row.public.id != id
            || row.public.revision == 0
            || row.public.revision > hagency_core::JSON_SAFE_MAX
            || row.public.applicability != Default::default()
        {
            return Err(Error::Schema);
        }
        Ok(row)
    })
    .transpose()
}
fn now() -> Result<u64, Error> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|v| u64::try_from(v.as_millis()).ok())
        .ok_or(Error::Unavailable)
}
fn sync_public(row: &mut Record) {
    row.public.has_credential = row.live.is_some();
    row.public.credential_kind = row.live.as_ref().map(|c| c.kind.clone());
    row.public.awaiting_install = row.pending.is_some();
    row.public.registration_available = row.registration.is_some();
}
fn credential_changed(row: &mut Record) {
    row.public.access_state = "unverified".into();
    row.public.checked_at = None;
}
impl CurrentManagementStore {
    pub(crate) async fn web_connections(
        &self,
        profile: &ProfileAuthority,
        after: Option<String>,
    ) -> Result<WebConnectionPage, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result=self.registry.0.store.call((16*MAX_RECORD) as u32,move|db|{
            backing.check(&authority)?;let tx=db.db.unchecked_transaction()?;
            let ids={let mut q=tx.prepare("SELECT entity FROM owner_web_catalog_settings WHERE profile=?1 AND kind=?2 AND entity>?3 ORDER BY entity LIMIT 17")?;
                q.query_map(params![authority.digest,KIND,after.unwrap_or_default()],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?};
            let items=ids.iter().take(16).map(|id|read(&tx,&authority.digest,id)?.map(|r|r.public).ok_or(Error::Schema)).collect::<Result<Vec<_>,Error>>()?;
            let next_after=(ids.len()>16).then(||items.last().expect("page").id.clone());
            backing.check(&authority)?;tx.commit()?;Ok(WebConnectionPage{items,next_after})
        }).await?;
        self.check(profile)?;
        Ok(result)
    }
    pub(crate) async fn web_connection_private(
        &self,
        profile: &ProfileAuthority,
        id: String,
    ) -> Result<WebConnectionPrivate, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result = self
            .registry
            .0
            .store
            .call(MAX_RECORD as u32, move |db| {
                backing.check(&authority)?;
                let row = read(&db.db, &authority.digest, &id)?.ok_or(Error::NotFound)?;
                backing.check(&authority)?;
                Ok(WebConnectionPrivate {
                    connection: row.public,
                    credential: row.live,
                    pending: row.pending,
                    registration: row.registration,
                })
            })
            .await?;
        self.check(profile)?;
        Ok(result)
    }
    pub(crate) async fn web_connection_result(
        &self,
        profile: &ProfileAuthority,
        actor: String,
        id: String,
    ) -> Result<Option<WebConnectionResult>, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result=self.registry.0.store.call(8192,move|db|{
            backing.check(&authority)?;
            let raw:Option<String>=db.db.query_row("SELECT result FROM management_commands WHERE actor_scope=?1 AND command_id=?2 AND operation='web.connection'",params![actor,id],|r|r.get(0)).optional()?;
            let result=raw.map(|s|serde_json::from_str::<Receipt>(&s)).transpose()?.filter(|r|r.profile==authority.digest).map(|r|r.result);
            backing.check(&authority)?;Ok(result)
        }).await?;
        self.check(profile)?;
        Ok(result)
    }
    pub(crate) async fn apply_web_connection(
        &self,
        profile: &ProfileAuthority,
        input: WebConnectionCommand,
        receipt: CommandReceipt,
    ) -> Result<WebConnectionResult, Error> {
        self.check(profile)?;
        input.validate()?;
        let backing = self.clone();
        let authority = profile.clone();
        let result=self.registry.0.store.call((2*MAX_RECORD) as u32,move|db|{
            backing.check(&authority)?;
            let tx=db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            backing.check(&authority)?;
            if let Some(original)=receipt.lookup::<Receipt>(&tx)?{
                if original.profile!=authority.digest{return Err(Error::LocalAuthority)}
                backing.check(&authority)?;return Ok(original.result)
            }
            let old=read(&tx,&authority.digest,&input.id)?;
            if old.as_ref().map_or(0,|r|r.public.revision)!=input.expected_revision{return Err(Error::Conflict)}
            if old.is_none(){
                let count:u64=tx.query_row("SELECT COUNT(*) FROM owner_web_catalog_settings WHERE profile=?1 AND kind=?2",params![authority.digest,KIND],|r|r.get(0))?;
                if count>=1000{return Err(Error::Capacity)}
            }
            let mut row=match old{
                Some(row)=>row,
                None=>{if !matches!(input.change,WebConnectionChange::Create{..}|WebConnectionChange::ImportConfiguration{..}){return Err(Error::NotFound)}
                    Record{public:WebConnection{id:input.id.clone(),label:String::new(),api_base_url:String::new(),revision:0,credential_kind:None,has_credential:false,awaiting_install:false,access_state:"unverified".into(),checked_at:None,allocated_tokens:None,registration_available:false,applicability:Default::default()},live:None,pending:None,registration:None,transport:None}}
            };
            match &input.change {
                WebConnectionChange::Create{label,api_base_url}=>{
                    if row.public.revision!=0{return Err(Error::Conflict)}
                    row.public.label=label.clone();row.public.api_base_url=api_base_url.clone();
                }
                WebConnectionChange::SetAllowance{allocated_tokens}=>row.public.allocated_tokens=*allocated_tokens,
                WebConnectionChange::ReplaceCredential{credential}=>{
                    row.live=Some(crate::Credential::from_web_configuration(credential)?);row.pending=None;
                    row.registration=None;row.transport=None;credential_changed(&mut row);
                }
                WebConnectionChange::WithdrawCredential=>{
                    row.live=None;row.pending=None;row.registration=None;row.transport=None;credential_changed(&mut row);
                }
                WebConnectionChange::IssueRegistration{request}=>{
                    let (credential,yaml)=request.private_web_registration()?;
                    if row.live.is_some(){row.pending=Some(credential)}else{row.live=Some(credential);credential_changed(&mut row)}
                    row.registration=Some(yaml);
                }
                WebConnectionChange::ImportConfiguration{label,api_base_url,credential,transport}=>{
                    row.public.label=label.clone();row.public.api_base_url=api_base_url.clone();
                    row.live=Some(crate::Credential::from_web_configuration(credential)?);row.pending=None;
                    row.registration=None;row.transport=Some(transport.clone());credential_changed(&mut row);
                }
                WebConnectionChange::Verification{state,credential_digest,staged}=>{
                    let credential=if *staged{row.pending.as_ref()}else{row.live.as_ref()}.ok_or(Error::State)?;
                    if web_connection_credential_digest(credential)?!=*credential_digest{return Err(Error::Conflict)}
                    if *staged && state=="accepted"{row.live=row.pending.take();}
                    row.public.access_state=state.clone();row.public.checked_at=Some(now()?);
                }
            }
            row.public.revision=row.public.revision.checked_add(1).filter(|r|*r<=hagency_core::JSON_SAFE_MAX).ok_or(Error::Capacity)?;
            sync_public(&mut row);
            let raw=serde_json::to_string(&row)?;if raw.len()>MAX_RECORD{return Err(Error::Capacity)}
            tx.execute("INSERT INTO owner_web_catalog_settings(profile,kind,entity,value) VALUES(?1,?2,?3,?4) ON CONFLICT(profile,kind,entity) DO UPDATE SET value=excluded.value",params![authority.digest,KIND,input.id,raw])?;
            let operation=match &input.change {
                WebConnectionChange::Create{..}=>"create", WebConnectionChange::SetAllowance{..}=>"allowance",
                WebConnectionChange::ReplaceCredential{..}=>"replace", WebConnectionChange::WithdrawCredential=>"withdraw",
                WebConnectionChange::IssueRegistration{..}=>"issue", WebConnectionChange::ImportConfiguration{..}=>"import",
                WebConnectionChange::Verification{..}=>"verify",
            }.to_owned();
            let result=WebConnectionResult{command_id:input.command_id,operation,connection:row.public};
            receipt.record(&tx,&Receipt{profile:authority.digest.clone(),result:result.clone()})?;
            backing.check(&authority)?;tx.commit()?;Ok(result)
        }).await?;
        self.check(profile)?;
        Ok(result)
    }
}
