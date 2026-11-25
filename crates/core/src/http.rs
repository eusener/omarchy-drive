//! Envio HTTP com autenticação, retry e backoff (429 / 5xx / rede).

use std::time::Duration;

use anyhow::{Result, bail};
use reqwest::{RequestBuilder, Response, StatusCode};

use crate::oauth::TokenManager;

/// Erro HTTP com status, para os provedores decidirem (ex.: 404, 410).
#[derive(Debug)]
pub struct HttpError {
    pub status: StatusCode,
    pub body: String,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let body: String = self.body.chars().take(500).collect();
        write!(f, "HTTP {}: {body}", self.status)
    }
}
impl std::error::Error for HttpError {}

pub fn status_of(e: &anyhow::Error) -> Option<StatusCode> {
    e.downcast_ref::<HttpError>().map(|h| h.status)
}

/// Executa a requisição construída por `build` (chamada a cada tentativa),
/// anexando o bearer token. Respostas não-2xx viram [`HttpError`].
pub async fn send(
    tokens: &TokenManager,
    mut build: impl FnMut() -> RequestBuilder,
) -> Result<Response> {
    let mut delay = Duration::from_millis(500);
    let mut refreshed = false;
    for attempt in 0..8 {
        let token = tokens.access_token().await?;
        let res = build().bearer_auth(token).send().await;
        match res {
            Ok(r) if r.status().is_success() => return Ok(r),
            Ok(r) if r.status() == StatusCode::UNAUTHORIZED && !refreshed => {
                tokens.invalidate().await;
                refreshed = true;
                continue;
            }
            Ok(r) => {
                let status = r.status();
                let retry_after = r
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(Duration::from_secs);
                let body = r.text().await.unwrap_or_default();
                // Google sinaliza limite de taxa com 403 + rateLimitExceeded.
                let rate_limited = status == StatusCode::TOO_MANY_REQUESTS
                    || (status == StatusCode::FORBIDDEN
                        && (body.contains("rateLimitExceeded")
                            || body.contains("userRateLimitExceeded")));
                if (rate_limited || status.is_server_error()) && attempt < 7 {
                    tokio::time::sleep(retry_after.unwrap_or(delay)).await;
                    delay = (delay * 2).min(Duration::from_secs(64));
                    continue;
                }
                return Err(HttpError { status, body }.into());
            }
            Err(e) if (e.is_connect() || e.is_timeout() || e.is_request()) && attempt < 7 => {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(64));
            }
            Err(e) => return Err(e.into()),
        }
    }
    bail!("número máximo de tentativas excedido")
}
