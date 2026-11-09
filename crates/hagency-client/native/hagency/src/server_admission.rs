//! Public, credential-free Hagency server admission shared by native clients.
use reqwest::Url;
use serde_json::Value;
use std::{fmt, time::Duration};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerMetadata {
    pub product: String,
    pub version: String,
    pub protocol_version: u64,
    pub capabilities: Vec<String>,
    pub homeserver: String,
    pub issuer: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionError {
    InvalidServer,
    Unavailable,
    RequestRejected,
    UnsupportedServer,
    UnsupportedProtocol,
    MissingCapabilities,
    InvalidMetadata,
}
impl AdmissionError {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidServer => "invalid_server",
            Self::Unavailable => "hagency_server_unavailable",
            Self::RequestRejected => "hagency_server_rejected",
            Self::UnsupportedServer => "unsupported_hagency_server",
            Self::UnsupportedProtocol => "unsupported_hagency_protocol",
            Self::MissingCapabilities => "unsupported_hagency_capabilities",
            Self::InvalidMetadata => "invalid_hagency_metadata",
        }
    }
    pub fn message(self) -> &'static str {
        match self {
            Self::InvalidServer => {
                "Enter a Hagency server HTTPS address (HTTP is allowed only on local loopback)."
            }
            Self::Unavailable => {
                "The Hagency server could not be reached. Check its address and connection, then retry."
            }
            Self::RequestRejected => {
                "The server rejected the Hagency discovery request. Check the server's public address and reverse proxy configuration."
            }
            Self::UnsupportedServer => {
                "Only Hagency servers are supported. Ordinary Matrix homeservers cannot be used to sign in."
            }
            Self::UnsupportedProtocol => {
                "This Hagency server uses an unsupported API version. Update the client or server."
            }
            Self::MissingCapabilities => {
                "This server does not provide the required Hagency OAuth and agent appservice capabilities."
            }
            Self::InvalidMetadata => {
                "The Hagency server metadata is invalid or does not match the requested server address."
            }
        }
    }
}
impl fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}
impl std::error::Error for AdmissionError {}
pub fn normalize_origin(value: &str) -> Result<Url, AdmissionError> {
    let url = Url::parse(value).map_err(|_| AdmissionError::InvalidServer)?;
    let loopback = url.host_str().is_some_and(|h| {
        h.eq_ignore_ascii_case("localhost")
            || h.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if value.len() > 2048
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || !(url.scheme() == "https" || url.scheme() == "http" && loopback)
    {
        return Err(AdmissionError::InvalidServer);
    }
    Ok(url)
}
pub fn validate_metadata(origin: &str, value: &Value) -> Result<ServerMetadata, AdmissionError> {
    let server = normalize_origin(origin)?;
    if value["product"] != "hagency-server" {
        return Err(AdmissionError::UnsupportedServer);
    }
    if value["protocolVersion"].as_u64() != Some(3) {
        return Err(AdmissionError::UnsupportedProtocol);
    }
    let version = value["version"]
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || ['.', '-', '+'].contains(&c))
        })
        .ok_or(AdmissionError::InvalidMetadata)?;
    let core = version.split(['-', '+']).next().unwrap_or("");
    let parts = core.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || parts.iter().any(|part| {
            part.is_empty()
                || !part.bytes().all(|b| b.is_ascii_digit())
                || part.len() > 1 && part.starts_with('0')
        })
    {
        return Err(AdmissionError::InvalidMetadata);
    }
    let capabilities = value["capabilities"]
        .as_array()
        .filter(|a| a.len() <= 64)
        .ok_or(AdmissionError::MissingCapabilities)?;
    let capabilities = capabilities
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|s| !s.is_empty() && s.len() <= 128)
                .map(str::to_owned)
                .ok_or(AdmissionError::InvalidMetadata)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if [
        "pasion-oauth",
        "owner-agent-appservice-v1",
        "global-agent-identity-v2",
        "execution-device-v1",
        "owner-direct-v1",
    ]
    .iter()
    .any(|required| !capabilities.iter().any(|c| c == required))
    {
        return Err(AdmissionError::MissingCapabilities);
    }
    let issuer = server
        .join("/auth/")
        .map_err(|_| AdmissionError::InvalidMetadata)?
        .to_string();
    if value["homeserver"] != server.as_str() || value["issuer"] != issuer {
        return Err(AdmissionError::InvalidMetadata);
    }
    Ok(ServerMetadata {
        product: "hagency-server".into(),
        version: version.into(),
        protocol_version: 2,
        capabilities,
        homeserver: server.to_string(),
        issuer,
    })
}
pub async fn discover(origin: &str) -> Result<ServerMetadata, AdmissionError> {
    let server = normalize_origin(origin)?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| AdmissionError::Unavailable)?;
    let mut response = client
        .get(
            server
                .join("/hagency/api/v1/discovery")
                .map_err(|_| AdmissionError::InvalidServer)?,
        )
        .send()
        .await
        .map_err(|_| AdmissionError::Unavailable)?;
    if response.status().is_server_error() {
        return Err(AdmissionError::Unavailable);
    }
    if matches!(response.status().as_u16(), 401 | 403) {
        return Err(AdmissionError::RequestRejected);
    }
    if response.status().as_u16() == 429 {
        return Err(AdmissionError::Unavailable);
    }
    if !response.status().is_success() {
        return Err(AdmissionError::UnsupportedServer);
    }
    let mut raw = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| AdmissionError::Unavailable)?
    {
        if raw.len() + chunk.len() > 16_384 {
            return Err(AdmissionError::InvalidMetadata);
        }
        raw.extend_from_slice(&chunk);
    }
    let value = serde_json::from_slice(&raw).map_err(|_| AdmissionError::UnsupportedServer)?;
    validate_metadata(server.as_str(), &value)
}
/// Stable storage namespace for the same embedded identity provider. This is
/// never used as an OAuth endpoint or accepted as an advertised issuer.
pub(crate) fn profile_storage_namespace(origin: &str) -> String {
    format!("{origin}_pasion/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn metadata() -> Value {
        json!({"product":"hagency-server","version":"0.1.0","protocolVersion":3,"capabilities":["pasion-oauth","owner-agent-appservice-v1","global-agent-identity-v2","execution-device-v1","owner-direct-v1"],"homeserver":"https://hagency.test/","issuer":"https://hagency.test/auth/"})
    }
    #[test]
    fn ordinary_matrix_and_service_identity_alone_cannot_gain_admission() {
        for value in [
            json!({"versions":["v1.12"]}),
            json!({"protocolVersion":3,"serviceMxid":"@hagency_appservice:test","homeserver":"https://hagency.test/","issuer":"https://hagency.test/auth/"}),
        ] {
            assert_eq!(
                validate_metadata("https://hagency.test/", &value),
                Err(AdmissionError::UnsupportedServer)
            );
        }
        assert_eq!(
            validate_metadata("https://hagency.test/", &metadata())
                .unwrap()
                .version,
            "0.1.0"
        );
    }
    #[test]
    fn admission_requires_supported_protocol_capabilities_and_same_origin_issuer() {
        let mut value = metadata();
        for version in [1, 2] {
            value["protocolVersion"] = json!(version);
            assert_eq!(
                validate_metadata("https://hagency.test/", &value),
                Err(AdmissionError::UnsupportedProtocol)
            );
        }
        value = metadata();
        value["capabilities"] = json!(["pasion-oauth"]);
        assert_eq!(
            validate_metadata("https://hagency.test/", &value),
            Err(AdmissionError::MissingCapabilities)
        );
        value = metadata();
        value["issuer"] = json!("https://foreign.test/auth/");
        assert_eq!(
            validate_metadata("https://hagency.test/", &value),
            Err(AdmissionError::InvalidMetadata)
        );
        value = metadata();
        value["homeserver"] = json!("https://foreign.test/");
        assert_eq!(
            validate_metadata("https://hagency.test/", &value),
            Err(AdmissionError::InvalidMetadata)
        );
        value = metadata();
        value["version"] = Value::Null;
        assert_eq!(
            validate_metadata("https://hagency.test/", &value),
            Err(AdmissionError::InvalidMetadata)
        );
    }
    #[test]
    fn plaintext_origins_are_only_explicit_loopback_not_local_network_names() {
        for origin in ["http://localhost/", "http://127.0.0.1/", "http://[::1]/"] {
            assert!(normalize_origin(origin).is_ok());
        }
        for origin in [
            "http://hagency.local/",
            "http://evil.localhost/",
            "http://192.168.1.1/",
            "https://u:p@hagency.test/",
            "https://hagency.test/?x=1",
        ] {
            assert!(normalize_origin(origin).is_err());
        }
    }
    #[tokio::test]
    #[ignore = "requires a reachable Hagency HTTPS server trusted by the standard system certificate store"]
    async fn installed_system_ca_https_hagency_admission_probe() {
        let origin =
            std::env::var("HAGENCY_ADMISSION_TEST_ORIGIN").expect("set HTTPS server origin");
        assert!(origin.starts_with("https://"));
        let metadata = discover(&origin)
            .await
            .expect("standard HTTPS certificate/hostname and Hagency metadata validation");
        assert_eq!(metadata.product, "hagency-server");
        assert_eq!(metadata.protocol_version, 2);
    }
    #[tokio::test]
    async fn public_http_admission_never_follows_redirects_or_reads_credentials_for_matrix() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (status, body, expected) in [
            (
                "200 OK",
                r#"{"versions":["v1.12"]}"#.to_string(),
                AdmissionError::UnsupportedServer,
            ),
            (
                "404 Not Found",
                "{}".into(),
                AdmissionError::UnsupportedServer,
            ),
            ("302 Found", "{}".into(), AdmissionError::UnsupportedServer),
            (
                "401 Unauthorized",
                r#"{"code":"native_origin_required"}"#.into(),
                AdmissionError::RequestRejected,
            ),
            (
                "403 Forbidden",
                r#"{"code":"origin_forbidden"}"#.into(),
                AdmissionError::RequestRejected,
            ),
            (
                "429 Too Many Requests",
                "{}".into(),
                AdmissionError::Unavailable,
            ),
            (
                "200 OK",
                " ".repeat(16_385),
                AdmissionError::InvalidMetadata,
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}/", listener.local_addr().unwrap());
            let task = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let n = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..n]);
                assert!(request.starts_with("GET /hagency/api/v1/discovery "));
                assert!(!request.to_lowercase().contains("authorization:"));
                let response = format!(
                    "HTTP/1.1 {status}\r\nLocation: https://foreign.invalid/\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            });
            assert_eq!(discover(&origin).await, Err(expected));
            task.await.unwrap();
        }
        struct ForbiddenSource;
        impl crate::native_owner::MatrixTokenSource for ForbiddenSource {
            fn access_token(
                &self,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                crate::native_owner::MatrixAccessToken,
                                crate::native_owner::NativeError,
                            >,
                        > + Send
                        + '_,
                >,
            > {
                panic!("ordinary Matrix must be rejected before touching SDK credentials");
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let read = socket.read(&mut request).await.unwrap();
            assert!(read > 0);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .await
                .unwrap();
        });
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let error = crate::native_owner::NativeOwner::open_with_matrix(
            &root.path().join("state"),
            &origin,
            "@owner:test",
            std::sync::Arc::new(ForbiddenSource),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.code, "unsupported_hagency_server");
        task.await.unwrap();
    }
}
