//! Context input on exact v3 work routes; historical descriptors keep their original bytes.
use super::*;
use super::context_io::ContextAction;
use super::budget_work_io::WorkAction;
use super::context_io::paired;
pub(super) async fn prepare(
    io: &Io,
    b: &Bound,
    r: &OwnerResourceRegistry,
    a: VerifiedSharedActivity,
    w: &SharedWorkItem,
    authority: SharedActivityRef,
    until: Instant,
) -> Result<SharedPreparedInput, NativeError> {
    if w.encrypted {
        let text=super::e2ee::input(io,b,w).await?;
        return r.prepare_shared_decrypted_input(a,w.clone(),text).await.map_err(store);
    }
    paired(b).await?;
    let mode: Timed<SharedWorkInputMode> = b
        .upload(
            WorkAction::Context(Box::new(ContextAction::Mode {
                work: w.id.clone(),
                input: SharedWorkEvidenceInput {
                    work_digest: digest(w)?,
                    runtime_command_digest: w.runtime_command_digest.clone(),
                },
            }))
            .into(),
            until,
        )
        .await?;
    mode.value.validate().map_err(store)?;
    let mode = mode.value;
    if mode.mode != "thread_context_v1" {
        return Err(failure(409, "native_context_protocol_required"));
    }
    let context = {
        let action = if let Some(hash) = &mode.context_digest {
            ContextAction::Recover {
                work: w.id.clone(),
                input: SharedContextRecoverInput {
                    work_digest: digest(w)?,
                    runtime_command_digest: w.runtime_command_digest.clone(),
                    context_digest: hash.clone(),
                },
            }
        } else {
            ContextAction::Freeze {
                work: w.id.clone(),
                input: SharedContextInput {
                    authority,
                    work_digest: digest(w)?,
                    version: 3,
                },
            }
        };
        let c: Timed<SharedContextRecord> = b.upload(WorkAction::Context(Box::new(action)).into(), until).await?;
        c.value.render_for(w).map_err(store)?;
        c.value
    };
    r.prepare_shared_input(a, w.clone(), mode, Some(context))
        .await
        .map_err(store)
}
pub(super) async fn start(
    b: &Bound,
    r: &OwnerResourceRegistry,
    attempt: SharedStartAttempt,
    w: &SharedWorkItem,
    input: &SharedPreparedInput,
    authority: SharedActivityRef,
    until: Instant,
) -> Result<SharedCallCustody, NativeError> {
    if let Some(c) = input.context() {
        let start: Timed<SharedContextStart> = b
            .upload(
                WorkAction::Context(Box::new(ContextAction::Start {
                    work: w.id.clone(),
                    input: SharedContextStartInput {
                        authority,
                        work_digest: digest(w)?,
                        context_digest: c.context_digest.clone(),
                        input_digest: c.input_digest.clone(),
                    },
                }))
                .into(),
                until,
            )
            .await?;
        r.issue_shared_context_call(attempt, start.value)
            .await
            .map_err(store)
    } else {
        if !w.encrypted {
            return Err(failure(409, "native_context_protocol_required"));
        }
        let start: Timed<SharedWorkStart> = b
            .upload(
                WorkAction::Start(SharedWorkInput {
                    authority,
                    work_id: w.id.clone(),
                    work_digest: digest(w)?,
                }).into(),
                until,
            )
            .await?;
        r.issue_shared_call(attempt, start.value)
            .await
            .map_err(store)
    }
}
