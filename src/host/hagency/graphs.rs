//! Closed graph services. Parse and validate before opening the native manager;
//! saved commands accept only their original native-generated command identity.
use ::hagency::native_owner::ManagementPermission;
use ::hagency::daemon::bridge::ManagementClient as NativeManagement;
#[cfg(feature = "resource-adoption")]
use ::hagency::native_owner::{
    OperatorGraphCancel, OperatorGraphCreate, OperatorGraphDetailQuery, OperatorGraphNodeQuery,
    OperatorGraphQuery, OperatorGraphUpdate, OperatorGraphRecipientOptionsQuery,
    OperatorGraphRecipientObserve, OperatorGraphBindRecipient,
};
#[cfg(feature = "resource-adoption")]
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(super) const LIST: &str = "hagency.graphs.list";
pub(super) const GET: &str = "hagency.graphs.get";
pub(super) const NODE: &str = "hagency.graphs.node";
pub(super) const CREATE: &str = "hagency.graphs.create";
pub(super) const UPDATE: &str = "hagency.graphs.update_node";
pub(super) const CANCEL: &str = "hagency.graphs.cancel";
pub(super) const REVIEW: &str = "hagency.graphs.command.get";
pub(super) const APPLY: &str = "hagency.graphs.command.apply";
pub(super) const RECIPIENT_GET: &str = "hagency.graphs.recipient.get";
pub(super) const RECIPIENT_OPTIONS: &str = "hagency.graphs.recipient.options";
pub(super) const RECIPIENT_OBSERVE: &str = "hagency.graphs.recipient.observe";
pub(super) const BIND_RECIPIENT: &str = "hagency.graphs.bind_recipient";

pub(super) fn permissions(service: &str) -> &'static [ManagementPermission] {
    use ManagementPermission::*;
    match service {
        LIST | GET | NODE => &[ReadOperatorGraphs],
        CREATE => &[ReadOperatorGraphs, CreateOperatorGraphs],
        UPDATE => &[ReadOperatorGraphs, UpdateOperatorGraphNodes],
        CANCEL => &[ReadOperatorGraphs, CancelOperatorGraphs],
        RECIPIENT_GET | RECIPIENT_OPTIONS | RECIPIENT_OBSERVE => &[
            ReadOperatorGraphs,
            ReadOperatorGraphRecipients,
            ReadProjectSetup,
            ReadProjects,
            ReadAgentRequests,
            ReadAgentStatus,
        ],
        BIND_RECIPIENT => &[
            ReadOperatorGraphs,
            ReadOperatorGraphRecipients,
            BindOperatorGraphRecipients,
            ReadProjectSetup,
            ReadProjects,
            ReadAgentRequests,
            ReadAgentStatus,
        ],
        // The saved command's original mutation grant is checked again natively.
        REVIEW | APPLY => &[ReadOperatorGraphs, ReadIntents],
        _ => &[],
    }
}

#[cfg(feature = "resource-adoption")]
pub(super) enum Call {
    List(OperatorGraphQuery),
    Detail(OperatorGraphDetailQuery),
    Node(OperatorGraphNodeQuery),
    Create(OperatorGraphCreate),
    Update(OperatorGraphUpdate),
    Cancel(OperatorGraphCancel),
    RecipientGet(OperatorGraphNodeQuery),
    RecipientOptions(OperatorGraphRecipientOptionsQuery),
    RecipientObserve(OperatorGraphRecipientObserve),
    BindRecipient(OperatorGraphBindRecipient),
    Review(String),
    Apply(String),
}

#[cfg(feature = "resource-adoption")]
fn invalid() -> String {
    "invalid_management_arguments".to_owned()
}
#[cfg(feature = "resource-adoption")]
fn parse<T: for<'de> Deserialize<'de>>(args: Value, max: usize) -> Result<T, String> {
    if !args.is_object() || serde_json::to_vec(&args).map_err(|_| invalid())?.len() > max {
        return Err(invalid());
    }
    serde_json::from_value(args).map_err(|_| invalid())
}
#[cfg(feature = "resource-adoption")]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CommandId {
    command_id: String,
}
#[cfg(feature = "resource-adoption")]
fn original_id(args: Value) -> Result<String, String> {
    let id = parse::<CommandId>(args, 16 * 1024)?.command_id;
    if !id.starts_with("graphcmd_")
        || id.len() != "graphcmd_".len() + 32
        || !id["graphcmd_".len()..]
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(invalid());
    }
    Ok(id)
}

#[cfg(feature = "resource-adoption")]
pub(super) fn call(service: &str, args: Value) -> Result<Option<Call>, String> {
    Ok(Some(match service {
        LIST => {
            let input: OperatorGraphQuery = parse(args, 16 * 1024)?;
            input.validate().map_err(|_| invalid())?;
            Call::List(input)
        }
        GET => {
            let input: OperatorGraphDetailQuery = parse(args, 16 * 1024)?;
            input.validate().map_err(|_| invalid())?;
            Call::Detail(input)
        }
        NODE => {
            let input: OperatorGraphNodeQuery = parse(args, 16 * 1024)?;
            input.validate().map_err(|_| invalid())?;
            Call::Node(input)
        }
        CREATE => {
            // JSON editor strings can escape again on the wire. Native
            // validation enforces the normalized 64 KiB creation limit.
            let input: OperatorGraphCreate = parse(args, rinx_miniapp_core::MAX_REQUEST_BYTES)?;
            input.validate().map_err(|_| invalid())?;
            Call::Create(input)
        }
        UPDATE => {
            let input: OperatorGraphUpdate = parse(args, rinx_miniapp_core::MAX_REQUEST_BYTES)?;
            input.validate().map_err(|_| invalid())?;
            Call::Update(input)
        }
        CANCEL => {
            let input: OperatorGraphCancel = parse(args, 16 * 1024)?;
            input.validate().map_err(|_| invalid())?;
            Call::Cancel(input)
        }
        RECIPIENT_GET => {
            let input: OperatorGraphNodeQuery = parse(args, 16 * 1024)?;
            input.validate().map_err(|_| invalid())?;
            Call::RecipientGet(input)
        }
        RECIPIENT_OPTIONS => {
            let input: OperatorGraphRecipientOptionsQuery = parse(args, 16 * 1024)?;
            input.validate().map_err(|_| invalid())?;
            Call::RecipientOptions(input)
        }
        RECIPIENT_OBSERVE => {
            let input: OperatorGraphRecipientObserve = parse(args, 16 * 1024)?;
            input.validate().map_err(|_| invalid())?;
            Call::RecipientObserve(input)
        }
        BIND_RECIPIENT => {
            let input: OperatorGraphBindRecipient = parse(args, 16 * 1024)?;
            input.validate().map_err(|_| invalid())?;
            Call::BindRecipient(input)
        }
        REVIEW => Call::Review(original_id(args)?),
        APPLY => Call::Apply(original_id(args)?),
        _ => return Ok(None),
    }))
}

#[cfg(feature = "resource-adoption")]
impl Call {
    pub(super) fn mutates(&self) -> bool {
        matches!(
            self,
            Self::Create(_)
                | Self::Update(_)
                | Self::Cancel(_)
                | Self::BindRecipient(_)
                | Self::Apply(_)
        )
    }
    pub(super) async fn execute(self, manager: &NativeManagement) -> Result<Value, String> {
        fn encode(result: impl Serialize) -> Result<Value, String> {
            serde_json::to_value(result).map_err(|_| "management_invalid_result".to_owned())
        }
        match self {
            Self::List(input) => encode(
                manager
                    .operator_graph_page(input)
                    .await
                    .map_err(|e| e.code)?,
            ),
            Self::Detail(input) => encode(
                manager
                    .operator_graph_detail(input)
                    .await
                    .map_err(|e| e.code)?,
            ),
            Self::Node(input) => encode(
                manager
                    .operator_graph_node(input)
                    .await
                    .map_err(|e| e.code)?,
            ),
            Self::Create(input) => encode(
                manager
                    .prepare_operator_graph_create(input)
                    .await
                    .map_err(|e| e.code)?,
            ),
            Self::Update(input) => encode(
                manager
                    .prepare_operator_graph_update(input)
                    .await
                    .map_err(|e| e.code)?,
            ),
            Self::Cancel(input) => encode(
                manager
                    .prepare_operator_graph_cancel(input)
                    .await
                    .map_err(|e| e.code)?,
            ),
            Self::RecipientGet(input) => encode(
                manager
                    .operator_graph_recipient_get(input)
                    .await
                    .map_err(|e| e.code)?,
            ),
            Self::RecipientOptions(input) => encode(
                manager
                    .operator_graph_recipient_options(input)
                    .await
                    .map_err(|e| e.code)?,
            ),
            Self::RecipientObserve(input) => encode(
                manager
                    .operator_graph_recipient_observe(input)
                    .await
                    .map_err(|e| e.code)?,
            ),
            Self::BindRecipient(input) => encode(
                manager
                    .prepare_operator_graph_bind_recipient(input)
                    .await
                    .map_err(|e| e.code)?,
            ),
            Self::Review(id) => encode(
                manager
                    .review_saved_operator_graph(id)
                    .await
                    .map_err(|e| e.code)?,
            ),
            Self::Apply(id) => encode(
                manager
                    .apply_saved_operator_graph(id)
                    .await
                    .map_err(|e| e.code)?,
            ),
        }
    }
}

#[cfg(all(test, feature = "resource-adoption"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn graph_host_rejects_authority_and_replacement_commands_before_initialization() {
        let valid = json!({"label":"Release", "nodes":{"test":{"assignee":"Recorded agent", "description":"Run tests"}}});
        assert!(matches!(
            call(CREATE, valid.clone()).unwrap(),
            Some(Call::Create(_))
        ));
        for field in [
            "id",
            "commandId",
            "owner",
            "profile",
            "scopeId",
            "roomId",
            "path",
            "runtimeDeliveryAuthorized",
        ] {
            let mut forged = valid.clone();
            forged[field] = json!("supplied");
            assert!(call(CREATE, forged).is_err(), "{field}");
        }
        let command = "graphcmd_0123456789abcdef0123456789abcdef";
        for service in [REVIEW, APPLY] {
            assert!(call(service, json!({"commandId":command})).is_ok());
            for field in ["change", "graphId", "expectedRevision", "owner", "result"] {
                let mut forged = json!({"commandId":command});
                forged[field] = json!("replacement");
                assert!(call(service, forged).is_err());
            }
            for id in ["", "../path", "taskcmd_0123456789abcdef0123456789abcdef"] {
                assert!(call(service, json!({"commandId":id})).is_err());
            }
        }
        assert!(call(GET, json!({"id":" graph"})).is_err());
        assert!(call(LIST, json!({"status":"bogus"})).is_err());
        assert!(call(CANCEL, json!({"id":"graph","expectedRevision":0})).is_err());
        assert!(call(NODE, json!({"id":"graph","nodeId":""})).is_err());
    }

    #[test]
    fn graph_host_preserves_large_json_and_rejects_ambiguous_editor_inputs() {
        let result = json!({"fraction":0.25,"text":"x".repeat(24000)});
        let mut args = json!({"id":"graph","expectedRevision":1,"patch":{"nodeId":"first","resultJson":result.to_string()}});
        assert!(call(UPDATE, args.clone()).is_ok());
        args["patch"]["result"] = Value::Null;
        assert!(call(UPDATE, args).is_err());
        for text in ["{bad", "{\"x\":1,\"x\":2}"] {
            assert!(call(UPDATE, json!({"id":"graph","expectedRevision":1,"patch":{"nodeId":"first","resultJson":text}})).is_err());
        }
        assert!(call(UPDATE, json!({"id":"graph","expectedRevision":1,"patch":{"nodeId":"first","resultJson":"null"}})).is_ok());
        assert!(call(UPDATE, json!({"id":"graph","expectedRevision":1,"patch":{"nodeId":"first","result":{"text":"x".repeat(65536)}}})).is_err());
    }

    #[test]
    fn recipient_host_accepts_only_selections_and_native_observation_digest() {
        let target =
            json!({"kind":"shared_agent","projectId":"project","agentRequestId":"request"});
        let observation = json!({"id":"graph","nodeId":"first","target":target});
        assert!(matches!(
            call(RECIPIENT_OBSERVE, observation.clone()).unwrap(),
            Some(Call::RecipientObserve(_))
        ));
        let prepared = json!({"id":"graph","nodeId":"first","expectedRevision":1,"target":target,"observationDigest":"a".repeat(64)});
        assert!(matches!(
            call(BIND_RECIPIENT, prepared.clone()).unwrap(),
            Some(Call::BindRecipient(_))
        ));
        for (service, input) in [
            (RECIPIENT_OBSERVE, observation),
            (BIND_RECIPIENT, prepared.clone()),
        ] {
            for field in [
                "owner",
                "profile",
                "scopeId",
                "commandId",
                "identityJson",
                "runtimeDeliveryAuthorized",
                "snapshot",
            ] {
                let mut forged = input.clone();
                forged[field] = json!("supplied");
                assert!(call(service, forged).is_err(), "{service}: {field}");
            }
            for field in [
                "puppetMxid",
                "roomId",
                "generation",
                "managerMxid",
                "canBind",
            ] {
                let mut forged = input.clone();
                forged["target"][field] = json!("supplied");
                assert!(call(service, forged).is_err(), "{service} target: {field}");
            }
        }
        for digest in ["", "original", &"A".repeat(64)] {
            let mut invalid = prepared.clone();
            invalid["observationDigest"] = json!(digest);
            assert!(call(BIND_RECIPIENT, invalid).is_err());
        }
        let mut stale = prepared;
        stale["expectedRevision"] = json!(0);
        assert!(call(BIND_RECIPIENT, stale).is_err());
        for kind in ["shared_agent", "personal_agent", "human"] {
            assert!(
                call(
                    RECIPIENT_OPTIONS,
                    json!({"id":"graph","nodeId":"first","kind":kind})
                )
                .is_ok()
            );
        }
        assert!(
            call(
                RECIPIENT_OPTIONS,
                json!({"id":"graph","nodeId":"first","kind":"shell"})
            )
            .is_err()
        );
        assert!(
            call(
                RECIPIENT_GET,
                json!({"id":"graph","nodeId":"first","target":target})
            )
            .is_err()
        );
    }

    #[test]
    fn graph_capabilities_do_not_grant_mutations_to_read_or_recovery() {
        use ManagementPermission::*;
        for service in [
            LIST,
            GET,
            NODE,
            REVIEW,
            APPLY,
            RECIPIENT_GET,
            RECIPIENT_OPTIONS,
            RECIPIENT_OBSERVE,
        ] {
            assert!(!permissions(service).contains(&CreateOperatorGraphs));
            assert!(!permissions(service).contains(&UpdateOperatorGraphNodes));
            assert!(!permissions(service).contains(&CancelOperatorGraphs));
            assert!(!permissions(service).contains(&BindOperatorGraphRecipients));
        }
        for (service, permission) in [
            (CREATE, CreateOperatorGraphs),
            (UPDATE, UpdateOperatorGraphNodes),
            (CANCEL, CancelOperatorGraphs),
        ] {
            assert_eq!(permissions(service), &[ReadOperatorGraphs, permission]);
        }
        assert!(permissions(BIND_RECIPIENT).contains(&BindOperatorGraphRecipients));
        for service in [
            RECIPIENT_GET,
            RECIPIENT_OPTIONS,
            RECIPIENT_OBSERVE,
            BIND_RECIPIENT,
        ] {
            assert!(permissions(service).contains(&ReadOperatorGraphRecipients));
            assert!(permissions(service).contains(&ReadProjectSetup));
            assert!(!permissions(service).contains(&ManageProjectSetup));
            assert!(!permissions(service).contains(&CreateOperatorGraphs));
            assert!(!permissions(service).contains(&UpdateOperatorGraphNodes));
        }
        for service in [
            LIST,
            GET,
            NODE,
            CREATE,
            UPDATE,
            CANCEL,
            REVIEW,
            APPLY,
            RECIPIENT_GET,
            RECIPIENT_OPTIONS,
            RECIPIENT_OBSERVE,
            BIND_RECIPIENT,
        ] {
            assert!(octosense_app_contract::hagency::SERVICES.contains(&service));
            assert!(octosense_app_contract::KNOWN_CAPABILITIES.contains(&service));
        }
    }
}

#[cfg(not(feature = "resource-adoption"))]
pub(super) struct Call;
#[cfg(not(feature = "resource-adoption"))]
pub(super) fn call(service: &str, _args: Value) -> Result<Option<Call>, String> {
    if [
        LIST,
        GET,
        NODE,
        CREATE,
        UPDATE,
        CANCEL,
        REVIEW,
        APPLY,
        RECIPIENT_GET,
        RECIPIENT_OPTIONS,
        RECIPIENT_OBSERVE,
        BIND_RECIPIENT,
    ]
    .contains(&service)
    {
        Err("graph_management_unavailable".into())
    } else {
        Ok(None)
    }
}
#[cfg(not(feature = "resource-adoption"))]
impl Call {
    pub(super) fn mutates(&self) -> bool {
        false
    }
    pub(super) async fn execute(self, _manager: &NativeManagement) -> Result<Value, String> {
        Err("graph_management_unavailable".into())
    }
}
