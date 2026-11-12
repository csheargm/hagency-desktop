//! Owner-scoped original console settings. Never calls Palpo approval APIs.
use super::*;
use crate::management::{CommandReceipt,WebCatalogQuery,WebCatalogView,WebCatalogAgent,WebCatalogChange,WebCatalogCommand,WebCatalogResult,WebRoleOffer,WebOfferPage,WebTrustEntry,WebTrustPage};
use serde_json::Value;

pub(super) fn setting(db:&Connection,profile:&str,kind:&str,entity:&str)->Result<Option<Value>,Error>{
    let raw:Option<String>=db.query_row("SELECT value FROM owner_web_catalog_settings WHERE profile=?1 AND kind=?2 AND entity=?3",params![profile,kind,entity],|r|r.get(0)).optional()?;
    raw.map(|v|serde_json::from_str(&v).map_err(Error::from)).transpose()
}
fn put(db:&Connection,profile:&str,kind:&str,entity:&str,value:&Value)->Result<(),Error>{
    let count:u64=db.query_row("SELECT COUNT(*) FROM owner_web_catalog_settings WHERE profile=?1 AND kind=?2",params![profile,kind],|r|r.get(0))?;
    if count>=256&&setting(db,profile,kind,entity)?.is_none(){return Err(Error::Capacity)}
    db.execute("INSERT INTO owner_web_catalog_settings(profile,kind,entity,value) VALUES(?1,?2,?3,?4) ON CONFLICT(profile,kind,entity) DO UPDATE SET value=excluded.value",params![profile,kind,entity,serde_json::to_string(value)?])?;Ok(())
}
fn owned(db:&Connection,profile:&ProfileAuthority)->Result<Vec<Resource>,Error>{
    let rows=db.prepare("SELECT r.config FROM resources r JOIN owner_resource_grants g ON g.resource_id=r.id WHERE g.profile=?1 AND g.active=1 ORDER BY r.id LIMIT 257")?.query_map([&profile.digest],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
    if rows.len()>256{return Err(Error::Capacity)}
    rows.into_iter().map(|v|{let r:Resource=serde_json::from_str(&v)?;authorize_resource(db,profile,&r.id())?;Ok(r)}).collect()
}
fn revision(db:&Connection,profile:&ProfileAuthority,resource:&str)->Result<String,Error>{
    // Explicit public kinds: connection credentials and private settings must
    // never be incorporated into a script-visible revision/read projection.
    let settings=db.prepare("SELECT kind,entity,value FROM owner_web_catalog_settings WHERE profile=?1 AND (kind IN('offer','trust') OR(kind IN('rate_cap','resource_default') AND entity=?2)) ORDER BY kind,entity LIMIT 521")?.query_map(params![profile.digest,resource],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<Result<Vec<_>,_>>()?;
    if settings.len()>520{return Err(Error::Capacity)}
    let resources=owned(db,profile)?.into_iter().map(|r|Ok((r.id(),crate::resource_publication_revision(&r)?))).collect::<Result<Vec<_>,Error>>()?;
    Ok(canonical::digest(&json!({"resource":resource,"resources":resources,"settings":settings}))?)
}
fn retire_blocker(db:&Connection,id:&str)->Result<Option<String>,Error>{
    let templates:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM agent_definitions WHERE resource_id=?1)",[id],|r|r.get(0))?;
    if templates{return Ok(Some("agent_templates_exist".into()))}
    let used:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM engagements WHERE resource_id=?1 AND state IN('pending','reserved','active'))",[id],|r|r.get(0))?;
    if used {return Ok(Some("active_agents".into()))}
    // Which grants still count is the capacity chain's question: one replaced
    // within its month stops counting once the grant that replaced it is closed.
    if let Some(reason)=crate::domain::owner_resources::capacity::retirement_grant_blocker(db,id)? {return Ok(Some(reason.into()))}
    let mut children=db.prepare("SELECT a.id,a.state,a.permanent_fence FROM owner_managed_agents a WHERE a.pool_grant_id IN(SELECT id FROM owner_capacity_grants WHERE resource_id=?1) OR a.project_reservation_id IN(SELECT p.id FROM owner_project_reservations p JOIN owner_capacity_grants g ON g.id=p.grant_id WHERE g.resource_id=?1)")?;
    for row in children.query_map([id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,bool>(2)?)))? {
        let (child,state,fenced)=row?;
        if !fenced || state!="fenced" {return Ok(Some("active_agents".into()))}
        // Provider usage may arrive after the process stopped. Keep that
        // liability intact, while requiring actual supervisor exit evidence
        // for every original process and successor before hiding the pool.
        let uncertain:bool=db.query_row("SELECT EXISTS(
            SELECT 1 FROM owner_managed_effects WHERE child_id=?1 AND state IN('issued','unknown')
            UNION ALL SELECT 1 FROM owner_shared_runtime_lifetimes l WHERE l.child_id=?1 AND NOT EXISTS(SELECT 1 FROM owner_shared_process_observations o WHERE o.child_id=l.child_id AND json_extract(o.payload,'$.state')='stopped')
            UNION ALL SELECT 1 FROM owner_shared_successor_attempts a WHERE a.child_id=?1 AND NOT EXISTS(SELECT 1 FROM owner_shared_successor_observations o WHERE o.attempt_id=a.id AND json_extract(o.payload,'$.state')='stopped')
            UNION ALL SELECT 1 FROM owner_managed_runtime_lifetimes l WHERE l.child_id=?1 AND l.issued_revision IS NOT NULL AND NOT EXISTS(SELECT 1 FROM owner_managed_runtime_observations o WHERE o.child_id=l.child_id AND json_extract(o.payload,'$.record.state')='stopped')
        )",[&child],|r|r.get(0))?;
        if uncertain {return Ok(Some("runtime_not_stopped".into()))}
        let without_custody:bool=db.query_row("SELECT (
            EXISTS(SELECT 1 FROM owner_managed_effects WHERE child_id=?1 AND state='qualified')
            OR EXISTS(SELECT 1 FROM owner_managed_calls WHERE child_id=?1 AND state IN('issued','unknown'))
        ) AND NOT EXISTS(SELECT 1 FROM owner_shared_runtime_lifetimes WHERE child_id=?1 UNION ALL SELECT 1 FROM owner_managed_runtime_lifetimes WHERE child_id=?1 AND issued_revision IS NOT NULL)",[&child],|r|r.get(0))?;
        if without_custody {return Ok(Some("runtime_not_stopped".into()))}
    }
    Ok(None)
}
fn agents(db:&Connection,profile:&ProfileAuthority,after:Option<&str>)->Result<(Vec<WebCatalogAgent>,Option<String>),Error>{
    let mut rows=db.prepare("SELECT e.id,e.name,e.resource_id FROM engagements e JOIN owner_resource_grants g ON g.resource_id=e.resource_id WHERE g.profile=?1 AND g.active=1 AND COALESCE(e.preset_id,'')='' AND e.state='pending' AND e.id>?2 AND NOT EXISTS(SELECT 1 FROM effects f WHERE f.engagement_id=e.id) ORDER BY e.id LIMIT 17")?.query_map(params![profile.digest,after.unwrap_or("")],|r|Ok(WebCatalogAgent{id:r.get(0)?,name:r.get(1)?,resource_id:r.get(2)?}))?.collect::<Result<Vec<_>,_>>()?;
    let next=(rows.len()>16).then(||rows[15].id.clone());rows.truncate(16);Ok((rows,next))
}
fn offers(db:&Connection,profile:&ProfileAuthority)->Result<Vec<WebRoleOffer>,Error>{
    let resources=owned(db,profile)?;
    hagency_core::qualification::roles().map(|role|{
        let saved=setting(db,&profile.digest,"offer",role)?;
        let mut row:WebRoleOffer=match &saved {Some(value)=>serde_json::from_value(value.clone())?,None=>WebRoleOffer{role:role.into(),..Default::default()}};
        // Qualification is a fact about models, even before publication. The
        // outward catalogue separately retains publication/provisioning/budget
        // gates, so a qualified model is not mistaken for available capacity.
        let qualified:Vec<_>=resources.iter().filter(|r|hagency_core::qualification::qualifies(&r.profile(),role,None)).collect();
        let qualified_families=qualified.iter().filter_map(|r|hagency_core::qualification::model(&r.profile()).1).collect::<std::collections::BTreeSet<_>>();
        row.qualified=!qualified.is_empty()&&(!hagency_core::qualification::cross_family(role)||qualified_families.len()>=2);
        let available:Vec<_>=resources.iter().filter(|r|r.qualifies(role)).collect();
        let available_families=available.iter().filter_map(|r|hagency_core::qualification::model(&r.profile()).1).collect::<std::collections::BTreeSet<_>>();
        row.catalog_published=!available.is_empty()&&(!hagency_core::qualification::cross_family(role)||available_families.len()>=2)&&saved.as_ref().is_none_or(|_|row.published);
        Ok(row)
    }).collect()
}
// The original canonical receipt is either the original applied result or a
// cancellation marker. Both permanently consume the same command/digest key.
fn outcome(db:&Connection,receipt:&CommandReceipt)->Result<Option<Option<WebCatalogResult>>,Error>{
    let Some(value)=receipt.lookup::<Value>(db)? else{return Ok(None)};
    if value==json!({"cancelled":true}){return Ok(Some(None))}
    Ok(Some(Some(serde_json::from_value(value)?)))
}
impl CurrentManagementStore {
    pub(crate) async fn cancel_web_catalog(&self,profile:&ProfileAuthority,input:WebCatalogCommand,receipt:CommandReceipt)->Result<Option<WebCatalogResult>,Error>{
        self.check(profile)?;input.validate()?;let backing=self.clone();let authority=profile.clone();
        let result=self.registry.0.store.call(64*1024,move|db|{
            backing.check(&authority)?;let tx=db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let associated:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM owner_resource_grants WHERE profile=?1 AND resource_id=?2)",params![authority.digest,input.resource_id],|r|r.get(0))?;
            if !associated{return Err(Error::LocalAuthority)}
            if let Some(original)=outcome(&tx,&receipt)?{backing.check(&authority)?;tx.commit()?;return Ok(original)}
            // This transaction races the effect writer on the exact same key.
            // Once stored, even a lost intent-journal reply cannot allow apply.
            receipt.record(&tx,&json!({"cancelled":true}))?;
            backing.check(&authority)?;tx.commit()?;Ok(None)
        }).await?;self.check(profile).map_err(|_|Error::OutcomeUnknown)?;Ok(result)
    }

    pub(crate) async fn web_catalog(&self,profile:&ProfileAuthority,query:WebCatalogQuery)->Result<WebCatalogView,Error>{
        self.check(profile)?;query.validate()?;let backing=self.clone();let authority=profile.clone();
        let result=self.registry.0.store.call(64*1024,move|db|{
            backing.check(&authority)?;let tx=db.db.unchecked_transaction()?;authorize_resource(&tx,&authority,&query.resource_id)?;
            let resource=read_resource(&tx,&query.resource_id)?;let name=backing.name_for(&tx,&query.resource_id)?.name;
            let rate=setting(&tx,&authority.digest,"rate_cap",&query.resource_id)?.and_then(|v|v.as_i64());
            let (policy,web_search)=crate::domain::exec_policy::resource_defaults::read(&tx,&authority.digest,&query.resource_id)?;
            let blocker=retire_blocker(&tx,&query.resource_id)?;let (agents,next_after)=agents(&tx,&authority,query.after.as_deref())?;
            let result=WebCatalogView{resource_id:query.resource_id.clone(),resource_name:if name.is_empty(){resource.model}else{name},revision:revision(&tx,&authority,&query.resource_id)?,rate_cap_per_day:rate,rate_enforced:false,default_yolo:policy,default_web_search:web_search,can_retire:blocker.is_none(),retire_blocked_reason:blocker,agents,next_after};
            backing.check(&authority)?;tx.commit()?;Ok(result)
        }).await?;self.check(profile)?;Ok(result)
    }
    pub(crate) async fn web_offers(&self,profile:&ProfileAuthority,id:String)->Result<WebOfferPage,Error>{
        self.check(profile)?;let backing=self.clone();let authority=profile.clone();
        let result=self.registry.0.store.call(64*1024,move|db|{backing.check(&authority)?;let tx=db.db.unchecked_transaction()?;authorize_resource(&tx,&authority,&id)?;let resource=read_resource(&tx,&id)?;let resource_qualifications=hagency_core::qualification::roles().filter(|role|hagency_core::qualification::qualifies(&resource.profile(),role,None)).map(str::to_owned).collect();let result=WebOfferPage{resource_id:id.clone(),revision:revision(&tx,&authority,&id)?,items:offers(&tx,&authority)?,resource_qualifications,offers_editable:true,offer_editing_reason:None};backing.check(&authority)?;tx.commit()?;Ok(result)}).await?;self.check(profile)?;Ok(result)
    }
    pub(crate) async fn web_trust(&self,profile:&ProfileAuthority,query:WebCatalogQuery)->Result<WebTrustPage,Error>{
        self.check(profile)?;query.validate()?;let backing=self.clone();let authority=profile.clone();
        let result=self.registry.0.store.call(64*1024,move|db|{
            backing.check(&authority)?;let tx=db.db.unchecked_transaction()?;authorize_resource(&tx,&authority,&query.resource_id)?;
            let rows=tx.prepare("SELECT value FROM owner_web_catalog_settings WHERE profile=?1 AND kind='trust' AND entity>?2 ORDER BY entity LIMIT 17")?.query_map(params![authority.digest,query.after.unwrap_or_default()],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
            let mut items=rows.into_iter().map(|r|serde_json::from_str::<WebTrustEntry>(&r).map_err(Error::from)).collect::<Result<Vec<_>,_>>()?;let next_after=(items.len()>16).then(||items[15].project_room_id.clone());items.truncate(16);
            let result=WebTrustPage{resource_id:query.resource_id.clone(),revision:revision(&tx,&authority,&query.resource_id)?,items,next_after,server_auto_approval:false};backing.check(&authority)?;tx.commit()?;Ok(result)
        }).await?;self.check(profile)?;Ok(result)
    }
    pub(crate) async fn apply_web_catalog(&self,profile:&ProfileAuthority,input:WebCatalogCommand,receipt:CommandReceipt)->Result<WebCatalogResult,Error>{
        self.check(profile)?;input.validate()?;let backing=self.clone();let authority=profile.clone();
        let result=self.registry.0.store.call(64*1024,move|db|{
            backing.check(&authority)?;let tx=db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Current profile must still possess the exact resource association;
            // retirement only deactivates it. Original receipt survives retirement.
            let associated:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM owner_resource_grants WHERE profile=?1 AND resource_id=?2)",params![authority.digest,input.resource_id],|r|r.get(0))?;
            if !associated{return Err(Error::LocalAuthority)}
            if let Some(result)=outcome(&tx,&receipt)? {backing.check(&authority)?;return result.ok_or(Error::ManagementChangeCancelled)}
            authorize_resource(&tx,&authority,&input.resource_id)?;
            if revision(&tx,&authority,&input.resource_id)?!=input.expected_revision{return Err(Error::Conflict)}
            let resource=read_resource(&tx,&input.resource_id)?;
            let retired=matches!(input.change,WebCatalogChange::Retire{});
            match &input.change {
                WebCatalogChange::RateCap{rate_cap_per_day}=>put(&tx,&authority.digest,"rate_cap",&input.resource_id,&json!(rate_cap_per_day))?,
                WebCatalogChange::DefaultPolicy{yolo,web_search}=>{
                    hagency_core::execution::ExecutionPolicy{yolo:*yolo}.validate_for(&resource.framework)?;
                    let prior=setting(&tx,&authority.digest,"resource_default",&input.resource_id)?;
                    let web=web_search.or_else(||prior.as_ref().and_then(|v|v["webSearch"].as_bool())).unwrap_or(*yolo);
                    put(&tx,&authority.digest,"resource_default",&input.resource_id,&json!({"yolo":yolo,"webSearch":web}))?;
                },
                WebCatalogChange::Retire{}=>{
                    if retire_blocker(&tx,&input.resource_id)?.is_some(){return Err(Error::State)}
                    tx.execute("UPDATE owner_resource_grants SET active=0,revision=revision+1 WHERE profile=?1 AND resource_id=?2",params![authority.digest,input.resource_id])?;
                    tx.execute("UPDATE owner_resource_identity SET revision=revision+1 WHERE singleton=1",[])?;
                },
                WebCatalogChange::Offer{role,count,budget_cap_per_engagement,rate_cap,published}=>{
                    let qualified=offers(&tx,&authority)?.into_iter().find(|r|r.role==*role).is_some_and(|r|r.qualified);
                    if *published&&!qualified{return Err(Error::Unqualified)}
                    put(&tx,&authority.digest,"offer",role,&serde_json::to_value(WebRoleOffer{role:role.clone(),count:*count,budget_cap_per_engagement:*budget_cap_per_engagement,rate_cap:*rate_cap,published:*published,qualified,catalog_published:*published&&qualified})?)?;
                },
                WebCatalogChange::TrustAdd{project_room_id,display_name}=>{
                    put(&tx,&authority.digest,"trust",project_room_id,&serde_json::to_value(WebTrustEntry{project_room_id:project_room_id.clone(),display_name:display_name.clone()})?)?;
                },
                WebCatalogChange::TrustRemove{project_room_id}=>{
                    if tx.execute("DELETE FROM owner_web_catalog_settings WHERE profile=?1 AND kind='trust' AND entity=?2",params![authority.digest,project_room_id])?==0{return Err(Error::NotFound)}
                },
                WebCatalogChange::AssignPreset{engagement_id}=>{
                    // Only an unconfigured, unprovisioned original pending agent.
                    // Managed-agent approvals and existing allocations are immutable.
                    let row:Option<(String,String)>=tx.query_row("SELECT e.resource_id,COALESCE(e.preset_id,'') FROM engagements e WHERE e.id=?1 AND e.state='pending' AND NOT EXISTS(SELECT 1 FROM effects f WHERE f.engagement_id=e.id)",[engagement_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
                    let (old,preset)=row.ok_or(Error::State)?;authorize_resource(&tx,&authority,&old)?;if !preset.is_empty(){return Err(Error::State)}
                    if !resource.provisionable()||crate::domain::resource_ceiling::effective_ceiling_tokens(&tx,&resource)?.is_none(){return Err(Error::Unqualified)}
                    crate::domain::agent_lifecycle::rebind_resource_in_transaction(&tx,engagement_id,&resource.preset_id,graphs::now_ms()?)?;
                },
            }
            let result=WebCatalogResult{command_id:input.command_id,resource_id:input.resource_id.clone(),revision:revision(&tx,&authority,&input.resource_id)?,operation:input.change.operation().into(),retired,server_auto_approval:false};
            receipt.record(&tx,&result)?;backing.check(&authority)?;tx.commit()?;Ok(result)
        }).await?;self.check(profile).map_err(|_|Error::OutcomeUnknown)?;Ok(result)
    }
}
