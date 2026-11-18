//! Whose Agent another owner's Agent is and what it runs on, from its own
//! Matrix profile ([`hagency_core::agent_owner`]): the Agent publishes both
//! when it starts, so a member on another server knows them before the
//! Agent has posted a run card. The owner is read only for another server's
//! Agent and accepted only when it is a person on the Agent's own server;
//! this server's roster still wins where it has a row. The runtime is
//! display-only, read for any Agent that is not this account's.
//!
//! Reads never block drawing: [`published_by`] returns what is cached and
//! starts at most one read per Agent, whose reply posts [`AgentFactsChanged`].

use std::{collections::HashMap, sync::{LazyLock, Mutex}, time::{Duration, Instant}};
use makepad_widgets::Cx;
use matrix_sdk::{Client, RoomMemberships, ruma::{OwnedUserId, RoomId, UserId, api::client::profile::ProfileFieldName}};
use hagency_core::agent_owner::{OWNER_FIELD, RUNTIME_FIELD, verified_owner, verified_runtime};
use super::agent_facts::AgentFactsChanged;

/// How long a published owner is reused; an owner is set once per Agent.
const FOUND_TTL: Duration = Duration::from_secs(300);
/// How long "not published" is reused: an Agent that starts soon publishes.
const MISSING_TTL: Duration = Duration::from_secs(30);
/// A read still marked loading after this is taken as lost (cancelled task).
const LOADING_TTL: Duration = Duration::from_secs(20);
/// Upper bound of Agents read for one Members list.
const MAX_AGENTS: usize = 32;
/// How long the Members list waits for profiles before grouping with the
/// server listings alone.
const ENRICH_TIMEOUT: Duration = Duration::from_secs(8);

/// What an Agent's profile says about it; `None` when it names nothing
/// acceptable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Published {
    pub owner: Option<OwnedUserId>,
    /// "Claude", "Codex" or "Octos".
    pub runtime: Option<&'static str>,
}

#[derive(Default)]
struct Cache {
    account: Option<OwnedUserId>,
    owners: HashMap<OwnedUserId, (Instant, Published)>,
    loading: HashMap<OwnedUserId, Instant>,
}
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Default::default);

fn cache_for(account: &OwnedUserId) -> std::sync::MutexGuard<'static, Cache> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.account.as_ref() != Some(account) {
        *cache = Cache { account: Some(account.clone()), ..Default::default() };
    }
    cache
}

fn fresh(entry: &(Instant, Published)) -> bool {
    entry.0.elapsed() < if entry.1 == Published::default() { MISSING_TTL } else { FOUND_TTL }
}

/// Only another server's Agent is read: this server's Agents are in its own
/// listings, which already settled (or deliberately left out) their owners.
fn is_federated_agent(agent: &UserId, account: &UserId) -> bool {
    crate::hagency::backend::is_agent_invitation(agent) && agent.server_name() != account.server_name()
}

/// Marks `agent` loading unless a read for it is already in flight.
fn start_loading(cache: &mut Cache, agent: &UserId) -> bool {
    if cache.loading.get(agent).is_some_and(|at| at.elapsed() < LOADING_TTL) { return false; }
    cache.loading.insert(agent.to_owned(), Instant::now());
    true
}

/// What another owner's `agent` publishes, as last read; starts a read when
/// unknown or stale. Nothing for a person or until read; no owner for this
/// server's Agent (its listings settle that).
pub fn published_by(agent: &UserId) -> Published {
    let Some(account) = crate::sliding_sync::current_user_id() else { return Published::default() };
    if !crate::hagency::backend::is_agent_invitation(agent) { return Published::default(); }
    let mut cache = cache_for(&account);
    let cached = cache.owners.get(agent).cloned();
    if cached.as_ref().is_none_or(|entry| !fresh(entry)) && start_loading(&mut cache, agent) {
        let agent = agent.to_owned();
        crate::sliding_sync::spawn_async_task(async move {
            let published = match crate::sliding_sync::get_client() {
                Some(client) => read_published(&client, &agent, &account).await,
                None => Published::default(),
            };
            remember(&account, &agent, published);
            Cx::post_action(AgentFactsChanged);
        });
    }
    cached.map(|(_, published)| published).unwrap_or_default()
}

/// Both fields of `agent`'s profile as `account` may use them: an owner only
/// from another server's Agent (an unreadable owner counts as none here).
async fn read_published(client: &Client, agent: &UserId, account: &UserId) -> Published {
    let federated = is_federated_agent(agent, account);
    let owner = async { if federated { read(client, agent).await.ok().flatten() } else { None } };
    let (owner, runtime) = futures_util::join!(owner, read_runtime(client, agent));
    Published { owner, runtime }
}

fn remember(account: &OwnedUserId, agent: &OwnedUserId, published: Published) {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    // A reply for a signed-out account is dropped, never mixed into the next.
    if cache.account.as_ref() != Some(account) { return; }
    cache.loading.remove(agent);
    cache.owners.insert(agent.clone(), (Instant::now(), published));
}

/// One field of `agent`'s profile: `Ok(None)` when it publishes none.
async fn read_field(client: &Client, agent: &UserId, field: &str) -> Result<Option<serde_json::Value>, String> {
    match client.account().fetch_profile_field_of(agent.to_owned(), ProfileFieldName::from(field)).await {
        Ok(value) => Ok(value.map(|value| value.value().into_owned())),
        Err(error) if matches!(error.client_api_error_kind(), Some(matrix_sdk::ruma::api::error::ErrorKind::NotFound)) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// The runtime label `agent`'s profile names; `None` when it names none,
/// an unknown one, or could not be read (display-only).
pub async fn read_runtime(client: &Client, agent: &UserId) -> Option<&'static str> {
    let value = read_field(client, agent, RUNTIME_FIELD).await.ok()??;
    verified_runtime(agent.as_str(), &value).map(crate::hagency::agent_panel::mapping::harness_label)
}

/// The verified owner `agent`'s profile names, read from its server now:
/// `Ok(None)` when it publishes none (or an unverifiable one), `Err` when
/// its profile could not be read.
pub async fn read(client: &Client, agent: &UserId) -> Result<Option<OwnedUserId>, String> {
    let value = read_field(client, agent, OWNER_FIELD).await?;
    Ok(value.and_then(|value| verified_owner(agent.as_str(), &value)).and_then(|owner| OwnedUserId::try_from(owner).ok()))
}

/// Adds to the Members list's `owners` each joined federated Agent's
/// published owner; `true` when some profile could not be read in time.
pub async fn add_profile_owners(room: &RoomId, owners: &mut HashMap<OwnedUserId, OwnedUserId>) -> bool {
    let Some(client) = crate::sliding_sync::get_client() else { return true };
    let (Some(room), Some(account)) = (client.get_room(room), client.user_id().map(ToOwned::to_owned)) else { return true };
    let Ok(members) = room.members_no_sync(RoomMemberships::JOIN).await else { return true };
    let agents: Vec<_> = members.iter().map(|m| m.user_id().to_owned())
        .filter(|id| is_federated_agent(id, &account) && !owners.contains_key(id)).take(MAX_AGENTS).collect();
    let reads = futures_util::future::join_all(agents.into_iter().map(|agent| {
        let (client, account) = (&client, &account);
        async move {
            let cached = cache_for(account).owners.get(&agent).filter(|entry| fresh(entry)).cloned();
            // Not cached from here: a read of the owner alone would hide
            // the runtime the profile publishes until the entry went stale.
            let owner = match cached {
                Some((_, published)) => Ok(published.owner),
                None => read(client, &agent).await,
            };
            (agent, owner)
        }
    }));
    let Ok(results) = tokio::time::timeout(ENRICH_TIMEOUT, reads).await else { return true };
    let unread = results.iter().any(|(_, owner)| owner.is_err());
    add_owners(owners, results.into_iter().filter_map(|(agent, owner)| Some((agent, owner.ok()??))));
    unread
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
    let unread = add_profile_owners(room, &mut owners).await;
    Ok((owners, incomplete || unread))
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
    fn only_another_servers_agent_is_read_and_one_read_at_a_time() {
        let anna = user_id!("@anna:alpha");
        assert!(is_federated_agent(user_id!("@_hagency_agt_b:beta"), anna));
        assert!(!is_federated_agent(user_id!("@_hagency_agt_a:alpha"), anna), "this server's listings settle its Agents");
        assert!(!is_federated_agent(user_id!("@bird:beta"), anna));
        let mut cache = Cache::default();
        let agent = user_id!("@_hagency_agt_b:beta");
        assert!(start_loading(&mut cache, agent));
        assert!(!start_loading(&mut cache, agent), "a read in flight is not repeated");
        cache.loading.insert(agent.to_owned(), Instant::now() - LOADING_TTL);
        assert!(start_loading(&mut cache, agent), "a lost read does not block the Agent forever");
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
        Mock::given(method("GET")).and(path_regex(r"/profile/(@|%40)_hagency_agt_down(:|%3A)beta/io\.hagency\.owner$"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({"errcode": "M_FORBIDDEN", "error": "profile lookup disabled"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(path_regex(r"/profile/(@|%40)_hagency_agt_b(:|%3A)beta/io\.hagency\.runtime$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"io.hagency.runtime": "claude"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(path_regex(r"/profile/(@|%40)_hagency_agt_x(:|%3A)beta/io\.hagency\.runtime$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"io.hagency.runtime": "gpt-9"})))
            .mount(&server).await;
        let client = signed_in(&server.uri()).await;
        assert_eq!(read(&client, user_id!("@_hagency_agt_b:beta")).await, Ok(Some(owned_user_id!("@bird:beta"))));
        assert_eq!(read(&client, user_id!("@_hagency_agt_x:beta")).await, Ok(None), "an owner off the Agent's server");
        assert_eq!(read(&client, user_id!("@_hagency_agt_n:beta")).await, Ok(None), "not published yet");
        assert!(read(&client, user_id!("@_hagency_agt_down:beta")).await.is_err(), "an unreadable profile is not 'none'");
        let anna = user_id!("@anna:alpha");
        assert_eq!(read_published(&client, user_id!("@_hagency_agt_b:beta"), anna).await,
            Published { owner: Some(owned_user_id!("@bird:beta")), runtime: Some("Claude") }, "owner and runtime from one profile");
        assert_eq!(read_runtime(&client, user_id!("@_hagency_agt_x:beta")).await, None, "an unknown runtime");
        assert_eq!(read_runtime(&client, user_id!("@_hagency_agt_n:beta")).await, None, "not published yet");
        assert_eq!(read_runtime(&client, user_id!("@_hagency_agt_down:beta")).await, None, "unreadable: nothing shown");
        let local = read_published(&client, user_id!("@_hagency_agt_b:beta"), user_id!("@bird:beta")).await;
        assert_eq!((local.owner, local.runtime), (None, Some("Claude")), "this server's Agent: runtime only, its listings settle the owner");
    }
}
