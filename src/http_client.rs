use std::{io::Read, time::Duration};

use anyhow::{Context, Result, bail};
use reqwest::{StatusCode, blocking::Client, redirect::Policy};
use serde_json::Value;

const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;

pub struct JsonResponse {
    pub status: StatusCode,
    pub body: Vec<u8>,
}

pub fn post_json(
    endpoint: &str,
    bearer_token: &str,
    connect_timeout_ms: u64,
    request_timeout_ms: u64,
    body: &Value,
) -> Result<JsonResponse> {
    if bearer_token.is_empty() {
        bail!("HTTP bearer credential is empty");
    }

    let client = Client::builder()
        .connect_timeout(Duration::from_millis(connect_timeout_ms.max(1)))
        .timeout(Duration::from_millis(request_timeout_ms.max(1)))
        .redirect(Policy::none())
        .build()
        .context("failed to build native HTTP client")?;

    let response = client
        .post(endpoint)
        .bearer_auth(bearer_token)
        .json(body)
        .send()
        .with_context(|| format!("native HTTP request to {endpoint} failed"))?;
    let status = response.status();
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("failed to read native HTTP response")?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        bail!("native HTTP response exceeded {MAX_RESPONSE_BYTES} bytes");
    }

    Ok(JsonResponse {
        status,
        body: bytes,
    })
}
