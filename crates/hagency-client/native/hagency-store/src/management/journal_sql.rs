use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
pub(super) fn authorize_sql(context: AuthContext<'_>) -> Authorization {
    if context.accessor.is_some() || context.database_name.is_some_and(|v| v != "main") {
        return Authorization::Deny;
    }
    let allowed = match context.action {
        AuthAction::Select | AuthAction::Transaction { .. } => true,
        AuthAction::Read { table_name, .. } => {
            matches!(table_name, "management_intents" | "management_commands")
        }
        AuthAction::Insert { table_name } => table_name == "management_intents",
        AuthAction::Update {
            table_name,
            column_name,
        } => {
            table_name == "management_intents" && matches!(column_name, "state" | "input" | "reply")
        }
        AuthAction::Function { function_name } => matches!(function_name, "count" | "length"),
        _ => false,
    };
    if allowed {
        Authorization::Allow
    } else {
        Authorization::Deny
    }
}
