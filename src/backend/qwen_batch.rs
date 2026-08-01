use std::{fs, path::Path};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::StatusCode;
use serde_json::{Value, json};
use url::Url;

use crate::{config::Config, http_client};

use super::text::apply_script_conversion;

pub fn transcribe_full_audio(config: &Config, wav_path: &Path) -> Result<Option<String>> {
    let base_url = resolve_base_url(config)?;
    let endpoint = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let audio_uri = wav_data_uri(wav_path)?;
    let body = json!({
        "model": config.asr.alibaba.final_pass_model,
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "input_audio",
                        "input_audio": {
                            "data": audio_uri,
                        }
                    }
                ]
            }
        ],
        "stream": false,
        "asr_options": {
            "language": config.asr.language.asr_code(),
            "enable_itn": config.asr.alibaba.final_pass_enable_itn,
        }
    });

    if config.asr.alibaba.api_key.trim().is_empty() {
        bail!("Alibaba final-pass ASR requires a configured credential");
    }
    let response = http_client::post_json(
        endpoint.as_str(),
        config.asr.alibaba.api_key.trim(),
        config.asr.connect_timeout_ms,
        config.asr.alibaba.final_pass_timeout_ms,
        &body,
    )?;

    parse_response(response.status, &response.body, config)
}

fn parse_response(status: StatusCode, body: &[u8], config: &Config) -> Result<Option<String>> {
    if status != StatusCode::OK {
        bail!(
            "Alibaba final-pass ASR returned HTTP {}: {}",
            status.as_u16(),
            truncate_for_error(&String::from_utf8_lossy(body))
        );
    }

    let payload: Value =
        serde_json::from_slice(body).context("failed to parse Alibaba final-pass ASR JSON")?;
    let Some(transcript) = payload["choices"][0]["message"]["content"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };

    apply_script_conversion(config.asr.language, transcript).map(Some)
}

fn wav_data_uri(wav_path: &Path) -> Result<String> {
    let bytes = fs::read(wav_path).with_context(|| {
        format!(
            "failed to read full-audio retranscription WAV `{}`",
            wav_path.display()
        )
    })?;
    Ok(format!("data:audio/wav;base64,{}", STANDARD.encode(bytes)))
}

fn resolve_base_url(config: &Config) -> Result<String> {
    let configured = config.asr.alibaba.final_pass_base_url.trim();
    if !configured.is_empty() {
        return Ok(configured.trim_end_matches('/').to_string());
    }

    let endpoint = Url::parse(config.asr.alibaba.endpoint.as_str()).with_context(|| {
        format!(
            "invalid Alibaba realtime endpoint `{}`",
            config.asr.alibaba.endpoint
        )
    })?;
    let host = endpoint
        .host_str()
        .ok_or_else(|| anyhow!("Alibaba realtime endpoint is missing a host"))?;

    let base_url = match host {
        "dashscope.aliyuncs.com" => "https://dashscope.aliyuncs.com/compatible-mode/v1",
        "dashscope-intl.aliyuncs.com" => "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
        "dashscope-us.aliyuncs.com" => "https://dashscope-us.aliyuncs.com/compatible-mode/v1",
        other => {
            bail!("cannot infer Alibaba final-pass base URL from realtime host `{other}`");
        }
    };

    Ok(base_url.to_string())
}

fn truncate_for_error(text: &str) -> String {
    const MAX_LEN: usize = 240;
    let mut shortened = text.chars().take(MAX_LEN).collect::<String>();
    if text.chars().count() > MAX_LEN {
        shortened.push('…');
    }
    shortened
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use super::{parse_response, resolve_base_url};
    use crate::config::{Config, Language};

    #[test]
    fn derives_cn_base_url_from_realtime_endpoint() {
        let config = Config::default();
        assert_eq!(
            resolve_base_url(&config).expect("base URL"),
            "https://dashscope.aliyuncs.com/compatible-mode/v1"
        );
    }

    #[test]
    fn derives_intl_base_url_from_realtime_endpoint() {
        let mut config = Config::default();
        config.asr.alibaba.endpoint = "wss://dashscope-intl.aliyuncs.com/api-ws/v1/realtime".into();
        assert_eq!(
            resolve_base_url(&config).expect("base URL"),
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1"
        );
    }

    #[test]
    fn parses_transcript_from_success_response() {
        let mut config = Config::default();
        // Keep response parsing independent of the optional OpenCC executable.
        config.asr.language = Language::English;
        let response = "{\"choices\":[{\"message\":{\"content\":\"请帮我把 Python JSON API 部署到 Kubernetes。\"}}]}";

        assert_eq!(
            parse_response(StatusCode::OK, response.as_bytes(), &config).expect("transcript"),
            Some("请帮我把 Python JSON API 部署到 Kubernetes。".into())
        );
    }

    #[test]
    fn treats_success_without_transcript_as_empty_audio() {
        let config = Config::default();
        let response = "{\"choices\":[{\"message\":{\"content\":\"  \"}}]}";

        assert_eq!(
            parse_response(StatusCode::OK, response.as_bytes(), &config).expect("empty response"),
            None
        );
    }
}
