# 0018. Ollama runtime: endpoint scheme and where usage caps live

Status: accepted, 2026-11-16

## Decision
- `LocalModel::server` accepts only `http://host:port`. https is refused at
  configuration time because the provenance check (`codex/ollama_verify.rs`)
  speaks plain HTTP; accepting https would fail every turn later instead.
- Usage caps for hosted Agents live server-side, in the concierge/owned
  runtime (hagency-client `hagency-agent-local/src/ollama_caps.rs`). The
  desktop copy had no production caller and was deleted.

## Consequences
A TLS-fronted Ollama host needs a plain-HTTP hop inside the operator's network.
