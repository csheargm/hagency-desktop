use super::{Error, Operation, Phase, SessionDriver};
use crate::claude::{TaskMcp, task_mcp::SERVER};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use tokio::{
    io::{AsyncRead, AsyncWrite},
};

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin, E: AsyncRead + Unpin> SessionDriver<R, W, E> {
    /// Bind once, after the host's actual dispatch/context acknowledgement. This
    /// does not manufacture that authority or expose a generic MCP control API.
    pub async fn bind_task_mcp(&mut self, helper: TaskMcp) -> Result<(), Error> {
        let operation = Operation::new(self, Phase::Ready)?;
        let result = operation.driver.bind_task_inner(helper).await;
        operation.finish(result)
    }
    async fn bind_task_inner(&mut self, helper: TaskMcp) -> Result<(), Error> {
        if !self.task_mcp_attempted {
            self.pass_buffered_hook_reports()
                .await
                .map_err(|_| Error::State)?;
        }
        if self.task_mcp_attempted || self.wire.has_buffered_input() {
            return Err(Error::State);
        }
        self.task_mcp_attempted = true;
        // All three correlated responses share one original lifetime/deadline.
        let until = self.wire.event_deadline();
        let before = self
            .mcp_control(
                "hagency-claude-mcp-before-1",
                json!({"subtype":"mcp_status"}),
                until,
            )
            .await?;
        let original = server_names(&before)?;
        // Native settings and plugin servers stay connected when the dynamic
        // MCP set changes. Never shadow an owner's same-named server.
        if original.contains(SERVER) {
            return Err(Error::TaskWriterStartup);
        }
        let owned = if self.room_mcp.is_some() {
            &[super::room_mcp::SERVER][..]
        } else {
            &[]
        };
        super::room_mcp::check_foreign_sdk_servers(&before, owned)?;
        let mut dynamic = json!({SERVER:helper.server()});
        if self.room_mcp.is_some() {
            dynamic[super::room_mcp::SERVER] = super::room_mcp::server_config();
        }
        let added = self
            .mcp_control(
                "hagency-claude-mcp-bind-1",
                json!({"subtype":"mcp_set_servers","servers":dynamic}),
                until,
            )
            .await?;
        if added.as_object().is_none_or(|o| o.len() != 3)
            || added["added"] != json!([SERVER])
            || added["removed"] != json!([])
            || added["errors"] != json!({})
        {
            return Err(Error::TaskWriterStartup);
        }
        let after = self
            .mcp_control(
                "hagency-claude-mcp-after-1",
                json!({"subtype":"mcp_status"}),
                until,
            )
            .await?;
        let after_names = server_names(&after)?;
        if !original.is_subset(&after_names) {
            return Err(Error::TaskWriterStartup);
        }
        self.verify_retained_room_server(&after)?;
        let server = servers(&after)?
            .iter()
            .find(|s| s["name"] == SERVER)
            .ok_or(Error::TaskWriterStartup)?;
        if server["name"] != SERVER || server["status"] != "connected" {
            return Err(Error::TaskWriterStartup);
        }
        let tools = server["tools"].as_array().ok_or(Error::TaskWriterStartup)?;
        let expected = helper.tools().into_iter().collect::<BTreeSet<_>>();
        if tools.len() != expected.len() {
            return Err(Error::TaskWriterStartup);
        }
        let actual = tools
            .iter()
            .map(|v| v["name"].as_str().ok_or(Error::TaskWriterStartup))
            .collect::<Result<BTreeSet<_>, _>>()?;
        if actual != expected {
            return Err(Error::TaskWriterStartup);
        }
        self.task_mcp = Some(helper);
        Ok(())
    }
}
pub(super) fn servers(value: &Value) -> Result<&Vec<Value>, Error> {
    if value.as_object().is_none_or(|o| o.len() != 1) {
        return Err(Error::TaskWriterStartup);
    }
    value["mcpServers"]
        .as_array()
        .filter(|v| v.len() <= 256)
        .ok_or(Error::TaskWriterStartup)
}
pub(super) fn server_names(value: &Value) -> Result<BTreeSet<&str>, Error> {
    let servers = servers(value)?;
    let names = servers
        .iter()
        .map(|s| {
            s["name"]
                .as_str()
                .filter(|name| {
                    !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
                })
                .ok_or(Error::TaskWriterStartup)
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if names.len() != servers.len() {
        return Err(Error::TaskWriterStartup);
    }
    Ok(names)
}
