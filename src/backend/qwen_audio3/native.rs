use std::{fs::File, io::Read, path::Path};

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::{
    config::{Audio3VocabularyTerm, Config, Language},
    http_client,
};

use super::super::text::apply_script_conversion;

pub(crate) const MAX_RAW_AUDIO_BYTES: usize = 10 * 1024 * 1024;

pub(crate) fn transcribe_full_audio(config: &Config, wav_path: &Path) -> Result<Option<String>> {
    let audio3 = &config.asr.alibaba_audio3;
    if audio3.api_key.trim().is_empty() {
        bail!("Qwen-Audio-3 native ASR requires a configured credential");
    }

    let wav_bytes = read_bounded_wav(wav_path)?;
    let body = request_body(
        &audio3.native_model,
        &wav_bytes,
        config.asr.language,
        audio3.language_hints_enabled,
        &audio3.vocabulary,
    )?;
    let response = http_client::post_json_sanitized(
        audio3.native_endpoint.as_str(),
        audio3.api_key.trim(),
        config.asr.connect_timeout_ms,
        audio3.native_timeout_ms,
        &body,
    )?;

    parse_response(response.status, &response.body, config)
}

fn read_bounded_wav(wav_path: &Path) -> Result<Vec<u8>> {
    let file = File::open(wav_path).with_context(|| {
        format!(
            "failed to open Qwen-Audio-3 full-audio WAV `{}`",
            wav_path.display()
        )
    })?;
    let mut bytes = Vec::new();
    file.take((MAX_RAW_AUDIO_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .context("failed to read Qwen-Audio-3 full-audio WAV")?;
    enforce_raw_audio_limit(bytes.len())?;
    Ok(bytes)
}

fn enforce_raw_audio_limit(byte_count: usize) -> Result<()> {
    if byte_count > MAX_RAW_AUDIO_BYTES {
        bail!("Qwen-Audio-3 native ASR input exceeds the 10 MiB raw audio limit");
    }
    Ok(())
}

fn request_body(
    model: &str,
    wav_bytes: &[u8],
    language: Language,
    language_hints_enabled: bool,
    vocabulary: &[Audio3VocabularyTerm],
) -> Result<Value> {
    enforce_raw_audio_limit(wav_bytes.len())?;
    let audio_uri = format!("data:audio/wav;base64,{}", STANDARD.encode(wav_bytes));
    let mut body = json!({
        "model": model,
        "input": {
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "input_audio",
                    "input_audio": {
                        "data": audio_uri
                    }
                }]
            }]
        },
        "parameters": {
            "format": "wav",
            "sample_rate": "16000"
        }
    });
    if language_hints_enabled {
        body["parameters"]["language_hints"] = super::language_hints(language);
    }
    if let Some(vocabulary) = super::vocabulary_value(vocabulary) {
        body["parameters"]["vocabulary"] = vocabulary;
    }
    Ok(body)
}

fn parse_response(status: StatusCode, body: &[u8], config: &Config) -> Result<Option<String>> {
    if status != StatusCode::OK {
        // The native API reports valid audio without recognized words as a
        // structured client error rather than a successful empty transcript.
        // Match only the documented provider marker and never expose the
        // untrusted response body to callers or logs.
        let no_words = status == StatusCode::BAD_REQUEST
            && serde_json::from_slice::<Value>(body)
                .ok()
                .and_then(|payload| payload["message"].as_str().map(str::to_owned))
                .as_deref()
                == Some("ASR_RESPONSE_HAVE_NO_WORDS");
        if no_words {
            return Ok(None);
        }

        // Do not include the provider body: it is untrusted and could echo audio,
        // credentials, or recognized text into an error that callers log.
        bail!("Qwen-Audio-3 native ASR returned HTTP {}", status.as_u16());
    }

    let payload: Value = serde_json::from_slice(body)
        .context("failed to parse Qwen-Audio-3 native ASR response JSON")?;
    let Some(transcript) = payload
        .pointer("/output/text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    else {
        return Ok(None);
    };

    apply_script_conversion(config.asr.language, transcript).map(Some)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
        thread,
        time::Duration,
    };

    use reqwest::StatusCode;
    use serde_json::{Value, json};

    use super::{
        MAX_RAW_AUDIO_BYTES, enforce_raw_audio_limit, parse_response, request_body,
        transcribe_full_audio,
    };
    use crate::config::{Audio3VocabularyTerm, Config, Language};

    #[test]
    fn request_body_matches_official_native_shape_and_data_uri() {
        assert_eq!(
            request_body(
                "qwen-audio-3.0-asr-flash",
                b"wav",
                Language::SimplifiedChinese,
                true,
                &[],
            )
            .unwrap(),
            json!({
                "model": "qwen-audio-3.0-asr-flash",
                "input": {
                    "messages": [{
                        "role": "user",
                        "content": [{
                            "type": "input_audio",
                            "input_audio": {
                                "data": "data:audio/wav;base64,d2F2"
                            }
                        }]
                    }]
                },
                "parameters": {
                    "format": "wav",
                    "sample_rate": "16000",
                    "language_hints": ["zh", "en"]
                }
            })
        );
    }

    #[test]
    fn raw_audio_limit_accepts_ten_mib_and_rejects_one_byte_more() {
        assert!(enforce_raw_audio_limit(MAX_RAW_AUDIO_BYTES).is_ok());
        assert!(enforce_raw_audio_limit(MAX_RAW_AUDIO_BYTES + 1).is_err());
        assert!(
            request_body(
                "model",
                &vec![0; MAX_RAW_AUDIO_BYTES + 1],
                Language::English,
                false,
                &[],
            )
            .is_err()
        );
    }

    #[test]
    fn native_controls_include_vocabulary_only_when_configured_and_never_heartbeat() {
        let disabled = request_body("model", b"wav", Language::English, false, &[]).unwrap();
        assert!(disabled["parameters"].get("language_hints").is_none());
        assert!(disabled["parameters"].get("vocabulary").is_none());
        assert!(disabled["parameters"].get("heartbeat").is_none());

        let vocabulary = [Audio3VocabularyTerm {
            term: " Voice Input ".into(),
            weight: 5,
        }];
        let configured =
            request_body("model", b"wav", Language::Korean, true, &vocabulary).unwrap();
        assert_eq!(
            configured["parameters"]["language_hints"],
            json!(["ko", "en"])
        );
        assert_eq!(
            configured["parameters"]["vocabulary"],
            json!({"Voice Input": 5})
        );
        assert!(configured["parameters"].get("heartbeat").is_none());
    }

    #[test]
    fn parses_successful_and_blank_native_responses() {
        let mut config = Config::default();
        config.asr.language = Language::English;

        assert_eq!(
            parse_response(
                StatusCode::OK,
                br#"{"output":{"text":"  native transcript  "}}"#,
                &config,
            )
            .unwrap(),
            Some("native transcript".into())
        );
        assert_eq!(
            parse_response(StatusCode::OK, br#"{"output":{"text":"  "}}"#, &config,).unwrap(),
            None
        );
        assert_eq!(
            parse_response(StatusCode::OK, br#"{"output":{}}"#, &config).unwrap(),
            None
        );
    }

    #[test]
    fn malformed_success_response_is_sanitized() {
        const SENTINEL: &str = "private-native-transcript-sentinel";
        let config = Config::default();
        let body = format!(r#"{{"output":{{"text":"{SENTINEL}"}} trailing"#);
        let error = parse_response(StatusCode::OK, body.as_bytes(), &config)
            .expect_err("malformed JSON must fail");

        assert!(
            error
                .to_string()
                .contains("failed to parse Qwen-Audio-3 native ASR response JSON")
        );
        assert!(!format!("{error:#}").contains(SENTINEL));
    }

    #[test]
    fn native_no_words_response_is_empty_audio() {
        let config = Config::default();
        assert_eq!(
            parse_response(
                StatusCode::BAD_REQUEST,
                br#"{"code":"CLIENT_ERROR","message":"ASR_RESPONSE_HAVE_NO_WORDS"}"#,
                &config,
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn native_error_response_is_sanitized() {
        const SENTINEL: &str = "private-native-error-body-sentinel";
        let config = Config::default();
        for body in [
            SENTINEL.as_bytes().to_vec(),
            format!(r#"{{"message":"{SENTINEL}"}}"#).into_bytes(),
        ] {
            let error = parse_response(StatusCode::BAD_REQUEST, &body, &config).unwrap_err();
            assert_eq!(
                error.to_string(),
                "Qwen-Audio-3 native ASR returned HTTP 400"
            );
            assert!(!format!("{error:#}").contains(SENTINEL));
        }
    }

    #[test]
    fn loopback_timeout_is_bounded_and_does_not_expose_request_details() {
        const QUERY_SENTINEL: &str = "private-endpoint-query-sentinel";
        const CREDENTIAL_SENTINEL: &str = "private-credential-sentinel";
        const AUDIO_SENTINEL: &str = "private-audio-sentinel";

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!(
            "http://{}/native?private={QUERY_SENTINEL}",
            listener.local_addr().unwrap()
        );
        let server = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_secs(1));
        });

        let temp = tempfile::tempdir().unwrap();
        let wav_path = temp.path().join("input.wav");
        std::fs::write(&wav_path, AUDIO_SENTINEL).unwrap();
        let mut config = Config::default();
        config.asr.alibaba_audio3.api_key = CREDENTIAL_SENTINEL.into();
        config.asr.alibaba_audio3.native_endpoint = endpoint.clone();
        config.asr.alibaba_audio3.native_timeout_ms = 300;

        let error = transcribe_full_audio(&config, &wav_path)
            .expect_err("server that never responds must time out");
        server.join().unwrap();
        let formatted = format!("{error:#}");
        assert!(formatted.contains("timed out"));
        assert!(!formatted.contains(&endpoint));
        for sentinel in [QUERY_SENTINEL, CREDENTIAL_SENTINEL, AUDIO_SENTINEL] {
            assert!(!formatted.contains(sentinel));
        }
    }

    #[test]
    fn loopback_request_uses_bearer_auth_and_exact_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/native", listener.local_addr().unwrap());
        let (request_tx, request_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            request_tx.send(request).unwrap();
            let response = br#"{"output":{"text":"loopback transcript"}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            )
            .unwrap();
            stream.write_all(response).unwrap();
        });

        let temp = tempfile::tempdir().unwrap();
        let wav_path = temp.path().join("input.wav");
        std::fs::write(&wav_path, b"wav").unwrap();
        let mut config = Config::default();
        config.asr.language = Language::Korean;
        config.asr.alibaba_audio3.api_key = "test-bearer-token".into();
        config.asr.alibaba_audio3.language_hints_enabled = true;
        config.asr.alibaba_audio3.heartbeat_enabled = true;
        config.asr.alibaba_audio3.vocabulary = vec![Audio3VocabularyTerm {
            term: "Voice Input".into(),
            weight: 5,
        }];
        config.asr.alibaba_audio3.native_endpoint = endpoint;
        config.asr.alibaba_audio3.native_model = "test-native-model".into();

        assert_eq!(
            transcribe_full_audio(&config, &wav_path).unwrap(),
            Some("loopback transcript".into())
        );
        let request = request_rx.recv().unwrap();
        server.join().unwrap();

        let (headers, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("POST /native HTTP/1.1\r\n"));
        assert!(
            headers
                .lines()
                .any(|line| line.eq_ignore_ascii_case("authorization: Bearer test-bearer-token"))
        );
        let body: Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["parameters"]["language_hints"], json!(["ko", "en"]));
        assert_eq!(body["parameters"]["vocabulary"], json!({"Voice Input": 5}));
        assert!(body["parameters"].get("heartbeat").is_none());
        assert_eq!(
            body,
            request_body(
                "test-native-model",
                b"wav",
                Language::Korean,
                true,
                &[Audio3VocabularyTerm {
                    term: "Voice Input".into(),
                    weight: 5,
                }],
            )
            .unwrap()
        );
    }

    fn read_http_request(stream: &mut impl Read) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        let header_end = loop {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        while bytes.len() < header_end + content_length {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(&buffer[..read]);
        }
        String::from_utf8(bytes[..header_end + content_length].to_vec()).unwrap()
    }
}
