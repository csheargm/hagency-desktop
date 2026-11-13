//! Future-agent defaults are independent of immutable resource/grant snapshots.
use crate::Error;
use hagency_core::execution::ExecutionPolicy;
use rusqlite::Connection;

/// Only new provisioning calls this. A shared resource with conflicting owner
/// defaults refuses ambiguity instead of silently granting unrestricted access.
pub(super) fn future_policy(db:&Connection,resource:&str)->Result<Option<ExecutionPolicy>,Error>{
    let values=db.prepare("SELECT DISTINCT json_extract(s.value,'$.yolo') FROM owner_web_catalog_settings s JOIN owner_resource_grants g ON g.profile=s.profile AND g.resource_id=s.entity WHERE s.kind='resource_default' AND s.entity=?1 AND g.active=1 LIMIT 2")?.query_map([resource],|r|r.get::<_,bool>(0))?.collect::<Result<Vec<_>,_>>()?;
    if values.len()>1{return Err(Error::Conflict)}
    Ok(values.first().map(|v|ExecutionPolicy{yolo:*v}))
}

fn role_offer(db: &Connection, profile: &str, role: &str) -> Result<Option<crate::management::WebRoleOffer>, Error> {
    use rusqlite::OptionalExtension;
    let raw: Option<String> = db.query_row(
        "SELECT value FROM owner_web_catalog_settings WHERE profile=?1 AND kind='offer' AND entity=?2",
        rusqlite::params![profile, role], |r| r.get(0),
    ).optional()?;
    let Some(raw) = raw else { return Ok(None) };
    let value: crate::management::WebRoleOffer = serde_json::from_str(&raw)?;
    if value.role != role { return Err(Error::Schema); }
    crate::management::WebCatalogChange::Offer {
        role: value.role.clone(), count: value.count,
        budget_cap_per_engagement: value.budget_cap_per_engagement,
        rate_cap: value.rate_cap, published: value.published,
    }.validate()?;
    Ok(Some(value))
}

/// Scope only prospective management catalogue reads. Existing resources and
/// grant revisions remain immutable; another owner cannot withdraw this offer.
pub(super) fn filter_capabilities(db: &Connection, profile: &str, capabilities: &mut Vec<String>) -> Result<(), Error> {
    let mut exposed = Vec::with_capacity(capabilities.len());
    for role in capabilities.iter() {
        if role_offer(db, profile, role)?.is_none_or(|offer| offer.published) {
            exposed.push(role.clone());
        }
    }
    *capabilities = exposed;
    Ok(())
}

/// Recheck prospective admission inside the existing reservation transaction.
/// Original receipts precede this check; mutable terms never fence an existing
/// child. Count includes reserved children, avoiding concurrent over-admission,
/// and excludes fenced/released children. Daily rates remain declared terms.
pub(super) fn managed_offer_refusal(db: &Connection, profile: &str, role: &str, requested_tokens: u64) -> Result<Option<&'static str>, Error> {
    let Some(offer) = role_offer(db, profile, role)? else { return Ok(None) };
    if !offer.published { return Ok(Some("resource_unavailable")); }
    if offer.budget_cap_per_engagement.is_some_and(|cap| requested_tokens > cap as u64) {
        return Ok(Some("insufficient_capacity"));
    }
    if let Some(limit) = offer.count {
        let held: u64 = db.query_row(
            "SELECT COUNT(*) FROM owner_managed_agents WHERE profile=?1 AND state='reserved' AND permanent_fence=0 AND json_extract(original_command,'$.definition.capability')=?2",
            rusqlite::params![profile, role], |r| r.get(0),
        )?;
        if held >= limit as u64 { return Ok(Some("insufficient_capacity")); }
    }
    Ok(None)
}
