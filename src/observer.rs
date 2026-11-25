use std::time::{Duration, SystemTime};

use reqwest::{header::RETRY_AFTER, Client, StatusCode};
use scraper::{Html, Selector};
use tokio::time::Instant;
use tracing::debug;
use url::Url;

use crate::closing_head_end;
use crate::targets::Target;

const MARKER_SELECTOR: &str = r#"meta[name="twitter:creator"][content="@discord"]"#;

#[derive(Debug)]
pub enum Observation {
    InvalidInviteCandidate,
    MarkerAbsent,
    RateLimited { retry_after: Option<Duration> },
    HttpError(StatusCode),
    NetworkError(String),
    BodyTooLarge,
}

pub fn build_client(timeout_secs: u64) -> Result<Client, reqwest::Error> {
    Client::builder()
        .user_agent("public-invite-monitor/0.2")
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(timeout_secs))
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(4)
        .tcp_nodelay(true)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?;
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date = httpdate::parse_http_date(value).ok()?;
    Some(date.duration_since(SystemTime::now()).unwrap_or_default())
}

pub async fn observe(
    client: &Client,
    target: &Target,
    max_body_bytes: usize,
) -> (Observation, Duration) {
    let started = Instant::now();
    let response = match client.get(target.url.clone()).send().await {
        Ok(response) => response,
        Err(error) => return (Observation::NetworkError(error.to_string()), started.elapsed()),
    };

    let status = response.status();
    if status == StatusCode::TOO_MANY_REQUESTS {
        return (
            Observation::RateLimited { retry_after: parse_retry_after(response.headers()) },
            started.elapsed(),
        );
    }
    if !status.is_success() {
        return (Observation::HttpError(status), started.elapsed());
    }
    if response.content_length().is_some_and(|n| n > max_body_bytes as u64) {
        return (Observation::BodyTooLarge, started.elapsed());
    }

    let mut response = response;
    let mut body = Vec::with_capacity(
        response.content_length().unwrap_or(0).min(max_body_bytes as u64).min(16 * 1024) as usize
    );

    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len().saturating_add(chunk.len()) > max_body_bytes {
                    return (Observation::BodyTooLarge, started.elapsed());
                }
                body.extend_from_slice(&chunk);
                if let Some(end) = closing_head_end(&body) {
                    body.truncate(end);
                    break;
                }
            }
            Ok(None) => break,
            Err(error) => return (Observation::NetworkError(error.to_string()), started.elapsed()),
        }
    }

    let html = String::from_utf8_lossy(&body);
    let document = Html::parse_document(&html);
    let selector = Selector::parse(MARKER_SELECTOR).expect("static metadata selector is valid");
    let result = if document.select(&selector).next().is_some() {
        Observation::InvalidInviteCandidate
    } else {
        Observation::MarkerAbsent
    };
    debug!(bytes_read = body.len(), "public invite page parsed");
    (result, started.elapsed())
}

pub fn invite_url(code: &str) -> Result<Url, url::ParseError> {
    Url::parse(&format!("https://discord.com/invite/{code}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_matches_expected_metadata() {
        let selector = Selector::parse(MARKER_SELECTOR).unwrap();
        let html = Html::parse_document(
            r#"<html><head><meta content="@discord" name="twitter:creator"></head></html>"#
        );
        assert!(html.select(&selector).next().is_some());
    }

    #[test]
    fn selector_rejects_unrelated_metadata() {
        let selector = Selector::parse(MARKER_SELECTOR).unwrap();
        let html = Html::parse_document(
            r#"<html><head><meta name="twitter:creator" content="@someone"></head></html>"#
        );
        assert!(html.select(&selector).next().is_none());
    }
}
