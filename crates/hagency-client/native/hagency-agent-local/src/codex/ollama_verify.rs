//! Provenance check before a model is used: Ollama's own catalogue must show
//! the model stored on that host. A cloud model is forwarded to ollama.com
//! even under a local alias, and `/api/tags` then carries `remote_host` /
//! `remote_model`; such a model, or one without tool support, is refused.
use super::{Error, Result, ollama::LocalModel};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const REMOTE_FIELDS: [&str; 2] = ["remote_host", "remote_model"];

fn remote(entry: &Value) -> bool {
    REMOTE_FIELDS.iter().any(|f| entry.get(*f).is_some_and(|v| !v.is_null()))
}
fn has_tools(entry: &Value) -> bool {
    entry["capabilities"].as_array().is_some_and(|c| c.iter().any(|v| v == "tools"))
}
/// Models stored on this host (not forwarded to a remote service). Tool
/// support is read from `/api/show`: older Ollama tags carry no capabilities.
pub fn stored_models(tags: &Value) -> Vec<String> {
    let mut names: Vec<String> = tags["models"].as_array().into_iter().flatten()
        .filter(|m| !remote(m))
        .filter_map(|m| m["name"].as_str())
        .filter(|n| super::ollama::validate_model(n).is_ok())
        .map(str::to_owned)
        .collect();
    names.sort();
    names
}
/// `tags` from `/api/tags`, `show` from `/api/show` for the same model.
pub fn check(tags: &Value, show: &Value, model: &str) -> Result<()> {
    super::ollama::validate_model(model)?;
    let wanted = if model.contains(':') { model.to_owned() } else { format!("{model}:latest") };
    let entry = tags["models"].as_array().into_iter().flatten()
        .find(|m| m["name"] == wanted.as_str() || m["model"] == wanted.as_str())
        .ok_or(Error::Profile("model is not installed on the Ollama host"))?;
    if remote(entry) || remote(show) {
        return Err(Error::Profile("model is served by a remote Ollama service"));
    }
    if !has_tools(show) {
        return Err(Error::Profile("model cannot call tools"));
    }
    Ok(())
}
/// Plain HTTP/1.0 JSON request (no chunking, connection closes).
async fn request(local: &LocalModel, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
    let authority = local.endpoint().strip_prefix("http://")
        .ok_or(Error::Profile("model check needs an http Ollama endpoint"))?;
    let host = authority.trim_start_matches('[').replace("]:", ":");
    let payload = body.map(Value::to_string).unwrap_or_default();
    let exchange = async {
        let mut stream = tokio::net::TcpStream::connect(host.as_str()).await?;
        stream.write_all(format!(
            "{method} {path} HTTP/1.0\r\nHost: {authority}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
            payload.len()).as_bytes()).await?;
        let mut response = Vec::new();
        stream.take(MAX_RESPONSE_BYTES).read_to_end(&mut response).await?;
        Ok::<_, std::io::Error>(response)
    };
    let response = tokio::time::timeout(REQUEST_TIMEOUT, exchange).await.map_err(|_| Error::Timeout)??;
    let split = response.windows(4).position(|w| w == b"\r\n\r\n")
        .ok_or(Error::Protocol("Ollama response has no headers"))?;
    let status = std::str::from_utf8(&response[..split]).ok().and_then(|h| h.split(' ').nth(1));
    if status != Some("200") {
        return Err(Error::Profile("model is not installed on the Ollama host"));
    }
    Ok(serde_json::from_slice(&response[split + 4..])?)
}
/// The picker's models: stored on the host and able to call tools.
pub async fn list_models(local: &LocalModel) -> Result<Vec<String>> {
    let tags = request(local, "GET", "/api/tags", None).await?;
    let mut usable = Vec::new();
    for model in stored_models(&tags) {
        let show = request(local, "POST", "/api/show", Some(&json!({"model": model}))).await?;
        if check(&tags, &show, &model).is_ok() {
            usable.push(model);
        }
    }
    Ok(usable)
}
pub async fn verify_model(local: &LocalModel, model: &str) -> Result<()> {
    let tags = request(local, "GET", "/api/tags", None).await?;
    let show = request(local, "POST", "/api/show", Some(&json!({"model": model}))).await?;
    check(&tags, &show, model)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tags() -> Value {
        json!({"models":[
            {"name":"qwen3:4b","model":"qwen3:4b"},
            {"name":"llama3.2:3b","model":"llama3.2:3b"},
            {"name":"alias:latest","model":"alias:latest","remote_host":"https://ollama.com","remote_model":"gpt-oss:120b","capabilities":["tools"]},
            {"name":"gpt-oss:120b-cloud","remote_host":"https://ollama.com","capabilities":["tools"]}]})
    }
    #[test]
    fn picker_candidates_exclude_remote_models() {
        assert_eq!(stored_models(&tags()), vec!["llama3.2:3b".to_string(), "qwen3:4b".to_string()]);
    }
    #[test]
    fn cloud_model_under_a_local_alias_is_refused() {
        let show = json!({"capabilities":["tools"]});
        assert!(check(&tags(), &show, "alias").is_err());
        assert!(check(&tags(), &show, "qwen3:4b").is_ok());
        assert!(check(&tags(), &json!({"capabilities":["tools"],"remote_host":"x"}), "qwen3:4b").is_err());
        assert!(check(&tags(), &json!({"capabilities":["completion"]}), "llama3.2:3b").is_err());
        assert!(check(&tags(), &show, "missing:1b").is_err());
    }
    #[tokio::test]
    async fn verify_talks_to_an_ollama_shaped_server() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            for _ in 0..2 {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut buf = vec![0; 4096];
                let n = s.read(&mut buf).await.unwrap();
                let body = if buf[..n].starts_with(b"GET /api/tags") { tags() } else { json!({"capabilities":["tools"]}) };
                s.write_all(format!("HTTP/1.0 200 OK\r\n\r\n{body}").as_bytes()).await.unwrap();
            }
        });
        let local = LocalModel::new(&format!("http://127.0.0.1:{port}")).unwrap();
        verify_model(&local, "qwen3:4b").await.unwrap();
    }
}
