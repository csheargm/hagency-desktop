//! Whose Agent another server's Agent is, from its own Matrix profile
//! ([`hagency_core::agent_owner`]): the Agent publishes its owner when it
//! starts, so a member on another server knows it before the Agent has
//! posted a run card. Accepted only when the owner is a person on the
//! Agent's own server; this server's roster still wins where it has a row.
//!
//! Reads never block drawing: [`owner_of`] returns what is cached and starts
//! at most one read per Agent, whose reply posts [`AgentFactsChanged`].

use std::{collections::{HashMap, HashSet}, sync::{LazyLock, Mutex}, time::{Duration, Instant}};
use makepad_widgets::Cx;
use matrix_sdk::{Client, RoomMemberships, ruma::{OwnedUserId, RoomId, UserId, api::client::profile::ProfileFieldName}};
use hagency_core::agent_owner::{OWNER_FIELD, verified_owner};
use super::agent_facts::AgentFactsChanged;

/// How long a published owner is reused; an owner is set once per Agent.
const FOUND_TTL: Duration = Duration::from_secs(300);
/// How long "not published" is reused: an Agent that starts soon publishes.
const MISSING_TTL: Duration = Duration::from_secs(30);
/// Upper bound of Agents read for one Members list.
const MAX_AGENTS: usize = 32;

#[derive(Default)]
struct Cache {
    account: Option<OwnedUserId>,
    owners: HashMap<OwnedUserId, (Instant, Option<OwnedUserId>)>,
    loading: HashSet<OwnedUserId>,
}
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Default::default);

fn cache_for(account: &OwnedUserId) -> std::sync::MutexGuard<'static, Cache> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.account.as_ref() != Some(account) {
        *cache = Cache { account: Some(account.clone()), ..Default::default() };
    }
    cache
}

fn fresh(entry: &(Instant, Option<OwnedUserId>)) -> bool {
    entry.0.elapsed() < if entry.1.is_some() { FOUND_TTL } else { MISSING_TTL }
}

/// The owner `agent`'s profile names, as last read; starts a read when it is
/// unknown or stale. `None` for a person or before the first read returns.
pub fn owner_of(agent: &UserId) -> Option<OwnedUserId> {
    if !crate::hagency::backend::is_agent_invitation(agent) { return None; }
    let account = crate::sliding_sync::current_user_id()?;
    let mut cache = cache_for(&account);
    let cached = cache.owners.get(agent).cloned();
    if cached.as_ref().is_none_or(|entry| !fresh(entry)) && cache.loading.insert(agent.to_owned()) {
        let agent = agent.to_owned();
        crate::sliding_sync::spawn_async_task(async move {
            let Some(client) = crate::sliding_sync::get_client() else { return };
            remember(&account, &agent, read(&client, &agent).await);
            Cx::post_action(AgentFactsChanged);
        });
    }
    cached.and_then(|(_, owner)| owner)
}

fn remember(account: &OwnedUserId, agent: &OwnedUserId, owner: Option<OwnedUserId>) {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    // A reply for a signed-out account is dropped, never mixed into the next.
    if cache.account.as_ref() != Some(account) { return; }
    cache.loading.remove(agent);
    cache.owners.insert(agent.clone(), (Instant::now(), owner));
}

/// The verified owner `agent`'s profile names, read from its server now.
pub async fn read(client: &Client, agent: &UserId) -> Option<OwnedUserId> {
    let field = ProfileFieldName::from(OWNER_FIELD);
    let value = match client.account().fetch_profile_field_of(agent.to_owned(), field).await {
        Ok(value) => value?,
        // Not published (404, the common case for an Agent not yet started)
        // or unreachable: the owner stays unknown here and the callers fall
        // back to the roster and run cards, which is all this read adds to.
        Err(error) => { makepad_widgets::log!("Agent owner profile unread for {agent}: {error}"); return None; }
    };
    OwnedUserId::try_from(verified_owner(agent.as_str(), &value.value())?).ok()
}

/// Agent → owner for a room's Members list: the server listings' `owners`,
/// plus each joined Agent they do not name whose own profile names its owner.
pub async fn add_profile_owners(room: &RoomId, owners: &mut HashMap<OwnedUserId, OwnedUserId>) {
    let Some(client) = crate::sliding_sync::get_client() else { return };
    let Some(room) = client.get_room(room) else { return };
    let Some(account) = client.user_id().map(ToOwned::to_owned) else { return };
    // An unread member list adds nothing; the listings' owners stand.
    let Ok(members) = room.members_no_sync(RoomMemberships::JOIN).await else { return };
    let agents = members.iter().map(|m| m.user_id().to_owned())
        .filter(|id| crate::hagency::backend::is_agent_invitation(id) && !owners.contains_key(id))
        .take(MAX_AGENTS);
    let mut found = Vec::new();
    for agent in agents.collect::<Vec<_>>() {
        let cached = cache_for(&account).owners.get(&agent).filter(|entry| fresh(entry)).cloned();
        let owner = match cached {
            Some((_, owner)) => owner,
            None => { let owner = read(&client, &agent).await; remember(&account, &agent, owner.clone()); owner }
        };
        found.extend(owner.map(|owner| (agent, owner)));
    }
    add_owners(owners, found);
}

/// Adds `found` Agent → owner pairs the listings did not name; a listing's
/// owner is never replaced.
pub fn add_owners(owners: &mut HashMap<OwnedUserId, OwnedUserId>, found: impl IntoIterator<Item = (OwnedUserId, OwnedUserId)>) {
    for (agent, owner) in found {
        if agent != owner { owners.entry(agent).or_insert(owner); }
    }
}

/// [`crate::home::mobile_chat_info::fetch_member_owners`] plus the owners
/// federated Agents publish, for grouping the Members list only. The Status
/// tab keeps reading the server listings alone (fail closed).
pub async fn member_owners(room: &RoomId, me: &OwnedUserId) -> Result<(HashMap<OwnedUserId, OwnedUserId>, bool), String> {
    let (mut owners, incomplete) = crate::home::mobile_chat_info::fetch_member_owners(room.as_str(), me).await?;
    add_profile_owners(room, &mut owners).await;
    Ok((owners, incomplete))
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::{owned_user_id, user_id};

    #[test]
    fn a_listing_owner_wins_and_an_agent_never_owns_itself() {
        let mut owners = HashMap::from([(owned_user_id!("@_hagency_agt_a:alpha"), owned_user_id!("@anna:alpha"))]);
        add_owners(&mut owners, [
            (owned_user_id!("@_hagency_agt_a:alpha"), owned_user_id!("@eve:alpha")),
            (owned_user_id!("@_hagency_agt_b:beta"), owned_user_id!("@bird:beta")),
            (owned_user_id!("@_hagency_agt_c:beta"), owned_user_id!("@_hagency_agt_c:beta")),
        ]);
        assert_eq!(owners[user_id!("@_hagency_agt_a:alpha")], user_id!("@anna:alpha"), "the roster's owner stands");
        assert_eq!(owners[user_id!("@_hagency_agt_b:beta")], user_id!("@bird:beta"), "a federated Agent gains its owner");
        assert!(!owners.contains_key(user_id!("@_hagency_agt_c:beta")));
    }

    #[test]
    fn a_missing_owner_is_asked_again_sooner_than_a_found_one() {
        let old = Instant::now() - Duration::from_secs(60);
        assert!(!fresh(&(old, None)), "an Agent may have started since");
        assert!(fresh(&(old, Some(owned_user_id!("@bird:beta")))));
    }

    async fn signed_in(origin: &str) -> Client {
        let client = Client::builder().homeserver_url(origin)
            .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_11]).build().await.unwrap();
        client.matrix_auth().restore_session(matrix_sdk::authentication::matrix::MatrixSession {
            meta: matrix_sdk::SessionMeta { user_id: owned_user_id!("@anna:alpha"), device_id: "D".into() },
            tokens: matrix_sdk::SessionTokens { access_token: "t".into(), refresh_token: None },
        }, matrix_sdk::store::RoomLoadSettings::default()).await.unwrap();
        client
    }

    /// User flow (Nov 18): Anna on alpha opens bird-claude's profile; her
    /// server relays the Agent's profile from beta, naming Bird.
    #[tokio::test]
    async fn a_federated_agents_profile_names_its_owner_only_on_its_server() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::{method, path_regex}};
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path_regex(r"/profile/(@|%40)_hagency_agt_b(:|%3A)beta/io\.hagency\.owner$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"io.hagency.owner": "@bird:beta"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(path_regex(r"/profile/(@|%40)_hagency_agt_x(:|%3A)beta/io\.hagency\.owner$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"io.hagency.owner": "@anna:alpha"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(path_regex(r"/profile/(@|%40)_hagency_agt_n(:|%3A)beta/io\.hagency\.owner$"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({"errcode": "M_NOT_FOUND", "error": "no"})))
            .mount(&server).await;
        let client = signed_in(&server.uri()).await;
        assert_eq!(read(&client, user_id!("@_hagency_agt_b:beta")).await, Some(owned_user_id!("@bird:beta")));
        assert_eq!(read(&client, user_id!("@_hagency_agt_x:beta")).await, None, "an owner off the Agent's server");
        assert_eq!(read(&client, user_id!("@_hagency_agt_n:beta")).await, None, "not published yet");
    }
}
