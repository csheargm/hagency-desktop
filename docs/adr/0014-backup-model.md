# 0014. Per-Agent backup model (Ollama, text-only)

Status: accepted (demo), 2026-11-15

## Context
A Codex Agent on a ChatGPT subscription fails every turn once its usage
limit is hit ("provider failed or interrupted the turn"). The owner wants the
request answered anyway, with a clear note that the main model was unavailable.

## Decision
- agent-local classifies a failed turn: Codex `turn.error.codexErrorInfo`
  (`usageLimitExceeded`, `serverOverloaded`, 5xx/connection codes) or message
  text; Claude's error result text (usage limit, 429, 529, 5xx). Only these,
  with zero tool requests in the turn, become `Error::Unavailable`. Denials,
  stops, timeouts and any failure after a tool request stay `Failed`.
- The host retries once, only on `Unavailable`, only when the Agent has a
  saved backup. The backup is an Ollama model (local or `-cloud`) reached at
  `http://127.0.0.1:11434/api/chat`, text-only: no tools, shell, files or
  GitHub, so no approval, outside-mode or merge-gate path is involved and no
  permission is added. No Hagency credential is sent; the request text (with
  its room context) is.
- The reply opens with "Main model unavailable (<reason>); answered by the
  backup model Ollama: <model> (cloud|local), without tools."
- The choice is saved per Agent in `<app data>/backup-models.json`, read at
  each request. Off unless chosen.

## Verified, not built: a tool-using backup
Ollama 0.40.15 serves an Anthropic-compatible `/v1/messages`. Claude Code with
`ANTHROPIC_BASE_URL=http://127.0.0.1:11434`, `ANTHROPIC_AUTH_TOKEN=ollama` and a
throwaway `CLAUDE_CONFIG_DIR` answered with `gpt-oss:120b-cloud` and used its
Write tool in a temp dir. Running Hagency's Claude runtime that way would keep
the same tool gate, but needs: per-spawn env in `runtime.rs` (it `env_clear`s),
an account check that accepts the Ollama endpoint, a fresh ledger call id (the
replay guard refuses the same one), run controls that can be re-claimed, and
host files re-bound on the new session. That is a split of `run_turn` first.
