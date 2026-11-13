//! Closed local task reads and native-saved mutation preparation/recovery.
use super::*;
use ::hagency::native_owner::{
    OperatorTaskQuery, OperatorTaskDetailQuery, OperatorBoardQuery, };
use ::hagency::daemon::bridge::ManagementClient as NativeManagement;
pub(super) const LIST: &str = "hagency.tasks.list";
pub(super) const GET: &str = "hagency.tasks.get";
pub(super) const BOARD: &str = "hagency.tasks.board";
pub(super) enum Read {
    List(OperatorTaskQuery),
    Detail(OperatorTaskDetailQuery),
    Board(OperatorBoardQuery),
}
pub(super) fn call(service: &str, args: Value) -> Result<Option<Read>, String> {
    let bad = |_| "invalid_management_arguments".to_owned();
    Ok(Some(match service {
        LIST => {
            let q: OperatorTaskQuery = parse(args)?;
            q.validate().map_err(bad)?;
            Read::List(q)
        }
        GET => {
            let q: OperatorTaskDetailQuery = parse(args)?;
            q.validate().map_err(bad)?;
            Read::Detail(q)
        }
        BOARD => {
            let q: OperatorBoardQuery = parse(args)?;
            q.validate().map_err(bad)?;
            Read::Board(q)
        }
        _ => return Ok(None),
    }))
}
impl Read {
    pub(super) async fn execute(self, m: &NativeManagement) -> Result<Value, String> {
        match self {
            Self::List(q) => value(m.operator_task_page(q).await.map_err(|e| e.code)?),
            Self::Detail(q) => value(m.operator_task_detail(q).await.map_err(|e| e.code)?),
            Self::Board(q) => value(m.operator_board(q).await.map_err(|e| e.code)?),
        }
    }
}
/// Each service accepts only its original task fields. IDs for new effects are
/// generated and saved by NativeManagement, never accepted from Splash.
pub(super) const CREATE: &str = "hagency.tasks.create";
pub(super) const EDIT: &str = "hagency.tasks.edit";
pub(super) const DELETE: &str = "hagency.tasks.delete";
pub(super) const ACCEPT: &str = "hagency.tasks.accept";
pub(super) const TRANSITION: &str = "hagency.tasks.transition";
pub(super) const COMMENT: &str = "hagency.tasks.comment";
pub(super) const REVIEW: &str = "hagency.tasks.command.get";
pub(super) const APPLY: &str = "hagency.tasks.command.apply";
pub(super) enum Write {
    Prepare(Box<::hagency::native_owner::OperatorTaskChange>),
    Review(String),
    Apply(String),
}
pub(super) fn write(service: &str, args: Value) -> Result<Option<Write>, String> {
    use ::hagency::native_owner::OperatorTaskChange as Change;
    let change = match service {
        CREATE => Change::Create(parse(args)?),
        EDIT => Change::Edit(parse(args)?),
        DELETE => Change::Delete(parse(args)?),
        ACCEPT => Change::Accept(parse(args)?),
        TRANSITION => Change::Transition(parse(args)?),
        COMMENT => Change::Comment(parse(args)?),
        REVIEW => return Ok(Some(Write::Review(original_id(args)?))),
        APPLY => return Ok(Some(Write::Apply(original_id(args)?))),
        _ => return Ok(None),
    };
    change
        .validate()
        .map_err(|_| "invalid_management_arguments".to_owned())?;
    Ok(Some(Write::Prepare(Box::new(change))))
}
fn original_id(args: Value) -> Result<String, String> {
    let id = parse::<CommandId>(args)?.command_id;
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err("invalid_management_arguments".into());
    }
    Ok(id)
}
impl Write {
    pub(super) fn mutates(&self) -> bool {
        !matches!(self, Self::Review(_))
    }
    pub(super) async fn execute(self, m: &NativeManagement) -> Result<Value, String> {
        match self {
            Self::Prepare(change) => {
                value(m.prepare_operator_task(*change).await.map_err(|e| e.code)?)
            }
            Self::Review(id) => value(m.review_saved_operator_task(id).await.map_err(|e| e.code)?),
            Self::Apply(id) => value(m.apply_saved_operator_task(id).await.map_err(|e| e.code)?),
        }
    }
}
#[cfg(test)]
mod writer_tests {
    use super::*;
    #[test]
    fn task_writer_host_closed_prepare_and_original_only_apply() {
        for field in [
            "commandId",
            "id",
            "author",
            "createdBy",
            "profile",
            "device",
            "path",
            "status",
            "createdAt",
        ] {
            let mut args = json!({"title":"Original"});
            args[field] = json!("forged");
            assert!(write(CREATE, args).is_err(), "{field}");
        }
        assert!(matches!(
            write(CREATE, json!({"title":"Original","labels":["one"]})).unwrap(),
            Some(Write::Prepare(_))
        ));
        assert!(
            write(
                EDIT,
                json!({"id":"task_original","expectedRevision":2,"assignee":null})
            )
            .is_ok()
        );
        assert!(
            write(
                COMMENT,
                json!({"id":"task_original","expectedRevision":2,"text":"Comment","author":"other"})
            )
            .is_err()
        );
        assert!(write(APPLY, json!({"commandId":"original"})).is_ok());
        for id in ["".to_owned(), "../path".to_owned(), "x".repeat(65)] {
            for service in [REVIEW, APPLY] {
                assert!(write(service, json!({"commandId":id})).is_err());
            }
        }
        assert!(write(APPLY, json!({"commandId":"original","expectedRevision":3})).is_err());
        assert!(write(REVIEW, json!({"commandId":"original","change":{}})).is_err());
        for service in [
            CREATE, EDIT, DELETE, ACCEPT, TRANSITION, COMMENT, REVIEW, APPLY,
        ] {
            assert!(octosense_app_contract::hagency::SERVICES.contains(&service));
            assert!(octosense_app_contract::KNOWN_CAPABILITIES.contains(&service));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operator_tasks_host_has_only_closed_read_inputs() {
        assert!(call(LIST, json!({"assignee":"Original name","offset":16})).is_ok());
        for field in [
            "path",
            "profile",
            "owner",
            "author",
            "commandId",
            "limit",
            "statusOverride",
        ] {
            let mut q = json!({});
            q[field] = json!("forged");
            assert!(call(LIST, q).is_err());
        }
        assert!(call(GET, json!({"id":"task_1","commentOffset":2})).is_ok());
        assert!(
            call(
                BOARD,
                json!({"projectId":"project_1","agentAfter":"Original name"})
            )
            .is_ok()
        );
        assert!(call(BOARD, json!({"agentAfter":"unscoped"})).is_err());
        assert!(
            call("hagency.tasks.delete", json!({"id":"task_1"}))
                .unwrap()
                .is_none()
        );
    }
}
