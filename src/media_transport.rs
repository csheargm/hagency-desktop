//! Bounded chat previews. File-save/download operations keep their own transport.
use matrix_sdk::{
    Client, Error,
    media::{MediaFormat, MediaRequestParameters},
    ruma::{
        api::{
            Metadata,
            client::{authenticated_media, media},
            path_builder::PathBuilder,
        },
        events::room::MediaSource,
    },
};
use std::{io::Read, time::Duration};

static FETCHES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
static REQUESTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(128);

#[derive(Debug, thiserror::Error)]
pub enum PreviewLimit {
    #[error("Image is too large to preview")]
    TooLarge,
    #[error("Image previews are busy")]
    Busy,
}
fn limit() -> Error {
    Error::UnknownError(Box::new(PreviewLimit::TooLarge))
}

/// Use the SDK's authenticated endpoint choice, HTTP/TLS client and encrypted
/// attachment verifier, but stop consuming the body at the preview limit.
/// SDK get_media_content otherwise buffers the entire response before returning.
pub async fn fetch(
    client: &Client,
    request: &MediaRequestParameters,
) -> matrix_sdk::Result<Vec<u8>> {
    fetch_with_timeout(client, request, Duration::from_secs(30)).await
}

async fn fetch_with_timeout(
    client: &Client,
    request: &MediaRequestParameters,
    timeout: Duration,
) -> matrix_sdk::Result<Vec<u8>> {
    fetch_with_current(
        client,
        request,
        timeout,
        crate::media_cache_reader::scope(client),
    )
    .await
}

async fn fetch_with_current(
    client: &Client,
    request: &MediaRequestParameters,
    timeout: Duration,
    current: crate::media_cache_reader::Current,
) -> matrix_sdk::Result<Vec<u8>> {
    tokio::time::timeout(timeout, fetch_inner(client, request, current))
        .await
        .map_err(|error| Error::UnknownError(Box::new(error)))?
}

fn cache_error(error: String) -> Error {
    Error::UnknownError(Box::new(std::io::Error::other(error)))
}
async fn fetch_inner(
    client: &Client,
    request: &MediaRequestParameters,
    current: crate::media_cache_reader::Current,
) -> matrix_sdk::Result<Vec<u8>> {
    if !current() {
        return Err(cache_error("Matrix session changed".into()));
    }
    let _request = REQUESTS
        .try_acquire()
        .map_err(|_| Error::UnknownError(Box::new(PreviewLimit::Busy)))?;
    let _permit = FETCHES
        .acquire()
        .await
        .map_err(|_| Error::UnknownError(Box::new(PreviewLimit::Busy)))?;
    let max = 8 * 1024 * 1024;
    let uri = crate::shared::attachment_download::media_source_mxc(&request.source);
    // Pending local echoes exist only in the SDK cache and are stored as File.
    let local = uri
        .server_name()
        .is_ok_and(|name| name == "send-queue.localhost");
    let cached_request = if local {
        MediaRequestParameters {
            source: request.source.clone(),
            format: MediaFormat::File,
        }
    } else {
        request.clone()
    };
    let cached =
        crate::media_cache_reader::read(client.clone(), cached_request, max, current.clone()).await;
    if local {
        if let Some(bytes) = cached.map_err(cache_error)? {
            return Ok(bytes);
        }
    } else if let Ok(Some(bytes)) = cached {
        return Ok(bytes);
    }
    // Remote cache reads are optional: an unknown/large SDK store is bypassed
    // before allocating its payload. The authenticated network path stays bounded.
    if local {
        return Err(Error::InsufficientData);
    }
    let supported = client.supported_versions().await?;
    let authenticated =
        authenticated_media::get_content::v1::Request::PATH_BUILDER.is_supported(&supported);
    let thumbnail = match (&request.source, &request.format) {
        (MediaSource::Plain(_), MediaFormat::Thumbnail(settings)) => Some(settings),
        _ => None,
    };
    let (server, media_id) = uri.parts().map_err(|_| Error::InsufficientData)?;
    if matches!(media_id, "." | "..") {
        return Err(Error::InsufficientData);
    }
    // Legacy servers require the deprecated media API; negotiated new servers use auth.
    #[allow(deprecated)]
    let history = match (authenticated, thumbnail.is_some()) {
        (true, true) => authenticated_media::get_content_thumbnail::v1::Request::PATH_BUILDER,
        (true, false) => authenticated_media::get_content::v1::Request::PATH_BUILDER,
        (false, true) => media::get_content_thumbnail::v3::Request::PATH_BUILDER,
        (false, false) => media::get_content::v3::Request::PATH_BUILDER,
    };
    let endpoint = history
        .make_endpoint_url(
            std::borrow::Cow::Borrowed(&supported),
            client.homeserver().as_str(),
            &[&server, &media_id],
            "",
        )
        .map_err(|error| Error::UnknownError(Box::new(error)))?;
    let mut url =
        url::Url::parse(&endpoint).map_err(|error| Error::UnknownError(Box::new(error)))?;
    if let Some(settings) = thumbnail {
        url.query_pairs_mut()
            .append_pair("width", &settings.width.to_string())
            .append_pair("height", &settings.height.to_string())
            .append_pair("method", settings.method.as_str())
            .append_pair("animated", if settings.animated { "true" } else { "false" });
    }
    let mut response = {
        let mut attempt = 0;
        loop {
            if !current() {
                return Err(cache_error("Matrix session changed".into()));
            }
            let mut builder = client
                .http_client()
                .get(url.clone())
                .timeout(Duration::from_secs(30));
            if authenticated {
                builder = builder
                    .bearer_auth(client.access_token().ok_or(Error::AuthenticationRequired)?);
            }
            let response = builder.send().await?;
            if !current() {
                return Err(cache_error("Matrix session changed".into()));
            }
            if attempt == 0
                && authenticated
                && response.status().as_u16() == 401
                && client.refresh_access_token().await.is_ok()
            {
                attempt += 1;
                continue;
            }
            if attempt < 2
                && (response.status().as_u16() == 429 || response.status().is_server_error())
            {
                let delay = response
                    .headers()
                    .get("retry-after")
                    .and_then(|header| header.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(1)
                    .min(10);
                drop(response);
                tokio::time::sleep(Duration::from_secs(delay)).await;
                attempt += 1;
                continue;
            }
            break response.error_for_status()?;
        }
    };
    if response
        .content_length()
        .is_some_and(|length| length > max as u64)
    {
        return Err(limit());
    }
    let mut content = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if !current() {
            return Err(cache_error("Matrix session changed".into()));
        }
        let next = content.len().saturating_add(chunk.len());
        if next > max {
            return Err(limit());
        }
        if next > content.capacity() {
            content.reserve_exact(next.next_power_of_two().min(max) - content.len());
        }
        content.extend_from_slice(&chunk);
    }
    if let MediaSource::Encrypted(file) = &request.source {
        let length = content.len();
        let mut cursor = std::io::Cursor::new(content);
        let mut reader = matrix_sdk_base::crypto::AttachmentDecryptor::new(
            &mut cursor,
            file.as_ref().clone().into(),
        )?;
        let mut decrypted = Vec::with_capacity(length);
        reader.read_to_end(&mut decrypted)?;
        if decrypted.len() > max {
            return Err(limit());
        }
        content = decrypted;
    }
    if !current() {
        return Err(cache_error("Matrix session changed".into()));
    }
    crate::media_cache_reader::write(client.clone(), request.clone(), &content, current.clone())
        .await
        .map_err(cache_error)?;
    if !current() {
        return Err(cache_error("Matrix session changed".into()));
    }
    Ok(content)
}

#[cfg(test)]
mod tests;
