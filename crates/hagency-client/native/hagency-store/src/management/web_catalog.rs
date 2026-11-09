//! Typed retained web-console controls. No caller supplies account or SQL scope.
use super::*;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct WebCatalogQuery { pub resource_id:String, pub after:Option<String> }
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag="operation",rename_all="snake_case",rename_all_fields="camelCase",deny_unknown_fields)]
pub enum WebCatalogChange {
    RateCap { rate_cap_per_day:Option<i64> },
    Retire {},
    AssignPreset { engagement_id:String },
    DefaultPolicy { yolo:bool, #[serde(default, skip_serializing_if="Option::is_none")] web_search:Option<bool> },
    Offer { role:String,count:Option<i64>,budget_cap_per_engagement:Option<i64>,rate_cap:Option<i64>,published:bool },
    TrustAdd { project_room_id:String,display_name:Option<String> },
    TrustRemove { project_room_id:String },
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct WebCatalogPrepare { pub resource_id:String,pub expected_revision:String,pub change:WebCatalogChange }
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct WebCatalogCommand { pub command_id:String,pub resource_id:String,pub expected_revision:String,pub change:WebCatalogChange }
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all="camelCase")]
pub struct WebCatalogAgent {pub id:String,pub name:String,pub resource_id:String}
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
#[serde(rename_all="camelCase")]
pub struct WebCatalogView {
    pub resource_id:String,pub resource_name:String,pub revision:String,
    pub rate_cap_per_day:Option<i64>,pub rate_enforced:bool,pub default_yolo:bool,
    #[serde(default)] pub default_web_search:bool,
    pub can_retire:bool,pub retire_blocked_reason:Option<String>,
    pub agents:Vec<WebCatalogAgent>,pub next_after:Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct WebRoleOffer {
    pub role:String,pub count:Option<i64>,pub budget_cap_per_engagement:Option<i64>,pub rate_cap:Option<i64>,
    pub published:bool,pub qualified:bool,pub catalog_published:bool,
}
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
#[serde(rename_all="camelCase")]
pub struct WebOfferPage {
    pub resource_id:String,pub revision:String,pub items:Vec<WebRoleOffer>,
    pub offers_editable:bool,pub offer_editing_reason:Option<String>,
    /// Model qualification of the selected resource, independent of publication,
    /// declared capacity and aggregate cross-family offer availability.
    pub resource_qualifications:Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct WebTrustEntry {pub project_room_id:String,pub display_name:Option<String>}
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
#[serde(rename_all="camelCase")]
pub struct WebTrustPage {pub resource_id:String,pub revision:String,pub items:Vec<WebTrustEntry>,pub next_after:Option<String>,pub server_auto_approval:bool}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct WebCatalogResult {pub command_id:String,pub resource_id:String,pub revision:String,pub operation:String,pub retired:bool,pub server_auto_approval:bool}

fn revision(v:&str)->Result<(),Error>{if v.len()!=64||!v.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)){return Err(hagency_core::InvalidInput("invalid revision").into())} Ok(())}
fn cap(v:Option<i64>)->Result<(),Error>{if v.is_some_and(|n|n<1||n>9_007_199_254_740_991){return Err(hagency_core::InvalidInput("invalid declared cap").into())}Ok(())}
fn room(v:&str)->Result<(),Error>{if v.len()>2048||!v.starts_with('!')||!v.contains(':')||v.split_once(':').is_none_or(|(a,b)|a.len()<2||b.is_empty())||v.chars().any(char::is_whitespace)||v.chars().any(char::is_control){return Err(hagency_core::InvalidInput("invalid room").into())}Ok(())}
impl WebCatalogQuery {pub fn validate(&self)->Result<(),Error>{identifier(&self.resource_id,128)?;if let Some(a)=&self.after {if a.len()>2048||a.chars().any(char::is_control){return Err(hagency_core::InvalidInput("invalid cursor").into())}}Ok(())}}
impl WebCatalogChange {
    pub fn operation(&self)->&'static str {match self {Self::RateCap{..}=>"rate_cap",Self::Retire{}=>"retire",Self::AssignPreset{..}=>"assign_preset",Self::DefaultPolicy{..}=>"default_policy",Self::Offer{..}=>"offer",Self::TrustAdd{..}=>"trust_add",Self::TrustRemove{..}=>"trust_remove"}}
    pub fn validate(&self)->Result<(),Error>{match self {
        Self::RateCap{rate_cap_per_day}=>cap(*rate_cap_per_day),
        Self::Retire{}|Self::DefaultPolicy{..}=>Ok(()),
        Self::AssignPreset{engagement_id}=>Ok(identifier(engagement_id,128)?),
        Self::Offer{role,count,budget_cap_per_engagement,rate_cap,..}=>{hagency_core::qualification::check_role(role)?;cap(*count)?;cap(*budget_cap_per_engagement)?;cap(*rate_cap)},
        Self::TrustAdd{project_room_id,display_name}=>{room(project_room_id)?;if display_name.as_ref().is_some_and(|s|s.chars().count()>256||s.chars().any(char::is_control)||s.trim()!=s){return Err(hagency_core::InvalidInput("invalid display name").into())}Ok(())},
        Self::TrustRemove{project_room_id}=>room(project_room_id),
    }}
}
impl WebCatalogPrepare {pub fn validate(&self)->Result<(),Error>{identifier(&self.resource_id,128)?;revision(&self.expected_revision)?;self.change.validate()}}
impl WebCatalogCommand {pub fn validate(&self)->Result<(),Error>{identifier(&self.command_id,128)?;WebCatalogPrepare{resource_id:self.resource_id.clone(),expected_revision:self.expected_revision.clone(),change:self.change.clone()}.validate()}}
impl Session {
    pub fn web_catalog_access(&self,context:&HostContext,write:bool)->Result<(),Error>{self.check(context,if write{Permission::ManageWebCatalog}else{Permission::ReadWebCatalog})}
    fn web_profile(&self,context:&HostContext,write:bool,store:&crate::owner_resources::adoption::CurrentManagementStore)->Result<crate::owner_resources::ProfileAuthority,Error>{
        self.web_catalog_access(context,write)?;let authority=self.authority();let context=context.clone();let permission=if write{Permission::ManageWebCatalog}else{Permission::ReadWebCatalog};
        self.current_profile(store)?.with_native_gate(move||authority.authorize(&context,permission).is_ok())
    }
    pub async fn web_catalog(&self,context:&HostContext,query:WebCatalogQuery)->Result<WebCatalogView,Error>{
        self.web_catalog_access(context,false)?;query.validate()?;
        let SessionStore::Current(store) = self.local_store()?;
        {let result=store.web_catalog(&self.web_profile(context,false,store)?,query).await?;self.web_catalog_access(context,false)?;return Ok(result)}
    }
    pub async fn web_offers(&self,context:&HostContext,resource_id:String)->Result<WebOfferPage,Error>{
        self.web_catalog_access(context,false)?;identifier(&resource_id,128)?;
        let SessionStore::Current(store) = self.local_store()?;
        {let result=store.web_offers(&self.web_profile(context,false,store)?,resource_id).await?;self.web_catalog_access(context,false)?;return Ok(result)}
    }
    pub async fn web_trust(&self,context:&HostContext,query:WebCatalogQuery)->Result<WebTrustPage,Error>{
        self.web_catalog_access(context,false)?;query.validate()?;
        let SessionStore::Current(store) = self.local_store()?;
        {let result=store.web_trust(&self.web_profile(context,false,store)?,query).await?;self.web_catalog_access(context,false)?;return Ok(result)}
    }
    /// Native original-ID cancellation. The canonical receipt arbitrates races
    /// and recovers applied effects without writing a contradictory cancellation.
    pub async fn cancel_web_catalog(&self,context:&HostContext,input:WebCatalogCommand)->Result<Option<WebCatalogResult>,Error>{
        self.web_catalog_access(context,true)?;input.validate()?;let _pending=self.scope.pending.try_acquire().map_err(|_|Error::Busy)?;let _command=self.scope.commands.lock().await;
        let SessionStore::Current(store) = self.local_store()?;
        {let receipt=CommandReceipt::new(context,"web.catalog",&input.command_id,&serde_json::json!({"profile":store.identity().0,"input":input}))?;let result=store.cancel_web_catalog(&self.web_profile(context,true,store)?,input,receipt).await?;self.web_catalog_access(context,true).map_err(|_|Error::OutcomeUnknown)?;return Ok(result)}
    }
    pub async fn apply_web_catalog(&self,context:&HostContext,input:WebCatalogCommand)->Result<WebCatalogResult,Error>{
        self.web_catalog_access(context,true)?;input.validate()?;let _pending=self.scope.pending.try_acquire().map_err(|_|Error::Busy)?;let _command=self.scope.commands.lock().await;
        let SessionStore::Current(store) = self.local_store()?;
        {let receipt=CommandReceipt::new(context,"web.catalog",&input.command_id,&serde_json::json!({"profile":store.identity().0,"input":input}))?;let result=store.apply_web_catalog(&self.web_profile(context,true,store)?,input,receipt).await?;self.web_catalog_access(context,true).map_err(|_|Error::OutcomeUnknown)?;return Ok(result)}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn web_catalog_closed_inputs_bound_numbers_and_unicode(){
        let parse=|value:serde_json::Value|serde_json::from_value::<WebCatalogChange>(value);
        for n in [0,-1,9_007_199_254_740_992i64] {assert!(parse(serde_json::json!({"operation":"rate_cap","rateCapPerDay":n})).unwrap().validate().is_err())}
        assert!(parse(serde_json::json!({"operation":"rate_cap","rateCapPerDay":null})).unwrap().validate().is_ok());
        assert!(parse(serde_json::json!({"operation":"default_policy","yolo":true,"owner":"@other:test"})).is_err());
        for room in ["room","!:server","!room:","!room:ser ver"]{assert!(parse(serde_json::json!({"operation":"trust_add","projectRoomId":room,"displayName":null})).unwrap().validate().is_err())}
        assert!(parse(serde_json::json!({"operation":"trust_add","projectRoomId":"!room:test","displayName":"中".repeat(256)})).unwrap().validate().is_ok());
        assert!(parse(serde_json::json!({"operation":"trust_add","projectRoomId":"!room:test","displayName":"中".repeat(257)})).unwrap().validate().is_err());
    }
}
