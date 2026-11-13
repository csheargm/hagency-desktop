//! A preflight selects context only. The admitted work is matched again before input.
use super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Reply {
    route: Option<SharedSessionRoute>,
}

impl Claimed {
    pub(super) async fn prepare_session(
        &self,
        io: &Io,
        launch: &launch::ClaudeLaunch,
        until: Instant,
    ) -> Result<SharedClaudeSession, NativeError> {
        let route = match self {
            Self::Root(_) => None,
            Self::Attempt(claimed) => {
                let (_, d) = &**claimed;
                route(
                    io,
                    &d.command.original_runtime_command,
                    &d.command.command_id,
                    &d.command_digest,
                    &d.token,
                    until,
                )
                .await
            }
            #[cfg(feature = "shared-budget-execution")]
            Self::Budget(claimed) => {
                let (_, d) = &**claimed;
                route(
                    io,
                    &d.command.original_runtime_command,
                    &d.command.command_id,
                    &d.command_digest,
                    &d.token,
                    until,
                )
                .await
            }
        };
        let settings = launch.session_settings_digest()?;
        match self {
            Self::Root(c) => c.0.prepare_claude_session(route, settings).await,
            Self::Attempt(c) => c.0.prepare_claude_session(route, settings).await,
            #[cfg(feature = "shared-budget-execution")]
            Self::Budget(c) => c.0.prepare_claude_session(route, settings).await,
        }
        .map_err(store)
    }
}
pub(in crate::console::native::shared_execution) async fn route(
    io: &Io,
    root: &SharedRuntimeCommand,
    id: &str,
    digest: &str,
    token: &str,
    until: Instant,
) -> Option<SharedSessionRoute> {
    // Older servers can still run a fresh context. A failed optional route must
    // never guess another member's session or delay startup beyond this bound.
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        let bound = io.bound().await?;
        io::binding(&bound.scope, root, true)?;
        let reply: Timed<Reply> = bound
            .upload(
                Action::SessionRoute {
                    id: id.into(),
                    input: SharedSessionRouteInput {
                        command_digest: digest.into(),
                        token: token.into(),
                    },
                },
                until,
            )
            .await?;
        Ok::<_, NativeError>(reply.value.route)
    })
    .await;
    match result {
        Ok(Ok(route)) => route,
        _ => {
            tracing::warn!("Shared native session route unavailable; opening a fresh context");
            None
        }
    }
}
