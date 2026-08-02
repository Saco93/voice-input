use std::{
    collections::BTreeMap,
    fs,
    io::{self, BufRead, BufReader, BufWriter, Read, Write},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    config::{Config, ConfigStore, LlmConfig, RevisionConflict, ValidationError},
    credentials::{self, ALIBABA_CREDENTIAL_ID, OPENROUTER_CREDENTIAL_ID},
    llm, paths,
    state::{Phase, Snapshot},
};

const PROTOCOL_VERSION: u32 = 1;
const MAX_REQUEST_LINE: usize = 1024 * 1024;
const SERVICE_PROBE_TIMEOUT: Duration = Duration::from_millis(750);

#[derive(Deserialize)]
struct Request {
    version: u32,
    id: i64,
    method: String,
    params: Value,
}

#[derive(Serialize)]
struct Response<T: Serialize> {
    version: u32,
    id: i64,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ProtocolError>,
}

#[derive(Debug, Serialize)]
struct ProtocolError {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    fields: Option<BTreeMap<String, String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveParams {
    revision: String,
    config: Value,
    #[serde(default)]
    credentials: BTreeMap<String, CredentialAction>,
    #[serde(default)]
    restart: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialAction {
    action: String,
    value: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LlmTestParams {
    llm: LlmConfig,
    credential: TestCredential,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TestCredential {
    source: String,
    value: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum ServiceState {
    Active,
    Activating,
    Deactivating,
    Inactive,
    Failed,
    Unknown,
}

#[derive(Serialize)]
struct ServiceStatus {
    state: ServiceState,
}

#[derive(Serialize)]
struct RuntimeStatus<'a> {
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<Phase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at_ms: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
}

#[derive(Serialize)]
struct RuntimeGetResult<'a> {
    service: ServiceStatus,
    runtime: RuntimeStatus<'a>,
}

pub fn run_stdio() -> Result<()> {
    let store = ConfigStore::new(paths::config_path()?);
    run_with_io(io::stdin().lock(), io::stdout().lock(), &store)
}

fn run_with_io<R: Read, W: Write>(reader: R, writer: W, store: &ConfigStore) -> Result<()> {
    let mut reader = BufReader::new(reader);
    let mut writer = BufWriter::new(writer);
    loop {
        let line = match read_line_limited(&mut reader) {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(LineError::TooLong) => {
                write_error(
                    &mut writer,
                    0,
                    ProtocolError {
                        code: "request_too_large",
                        message: "request line exceeds 1 MiB".into(),
                        fields: None,
                    },
                )?;
                continue;
            }
            Err(LineError::Io(error)) => {
                return Err(error).context("failed to read protocol input");
            }
        };
        if line.is_empty() {
            write_error(
                &mut writer,
                0,
                ProtocolError {
                    code: "invalid_request",
                    message: "request line must contain JSON".into(),
                    fields: None,
                },
            )?;
            continue;
        }
        let response = handle_line(&line, store);
        writer.write_all(&response)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    Ok(())
}

fn handle_line(line: &[u8], store: &ConfigStore) -> Vec<u8> {
    let parsed_value = serde_json::from_slice::<Value>(line);
    let id = parsed_value
        .as_ref()
        .ok()
        .and_then(|value| value.get("id"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let request = match parsed_value.and_then(serde_json::from_value::<Request>) {
        Ok(request) => request,
        Err(_) => return serialize_error(id, error("invalid_request", "invalid request envelope")),
    };
    if request.version != PROTOCOL_VERSION {
        return serialize_error(
            request.id,
            error(
                "unsupported_version",
                "only protocol version 1 is supported",
            ),
        );
    }

    match dispatch(&request, store) {
        Ok(result) => serde_json::to_vec(&Response {
            version: PROTOCOL_VERSION,
            id: request.id,
            ok: true,
            result: Some(result),
            error: None,
        })
        .expect("response serialization cannot fail"),
        Err(protocol_error) => serialize_error(request.id, protocol_error),
    }
}

fn dispatch(request: &Request, store: &ConfigStore) -> std::result::Result<Value, ProtocolError> {
    match request.method.as_str() {
        "settings.get" => {
            require_empty_object(&request.params)?;
            settings_get(store)
        }
        "settings.save" => {
            let params: SaveParams = serde_json::from_value(request.params.clone())
                .map_err(|_| error("invalid_params", "invalid settings.save parameters"))?;
            settings_save(store, params)
        }
        "runtime.get" => {
            require_empty_object(&request.params)?;
            Ok(runtime_get())
        }
        "llm.test" => {
            let params: LlmTestParams = serde_json::from_value(request.params.clone())
                .map_err(|_| error("invalid_params", "invalid llm.test parameters"))?;
            llm_test(store, params)
        }
        _ => Err(error("method_not_found", "unknown method")),
    }
}

fn runtime_get() -> Value {
    let service_state = probe_service_state();
    let snapshot = paths::runtime_dir()
        .ok()
        .and_then(|directory| fs::read(directory.join("state.json")).ok())
        .and_then(|contents| parse_runtime_snapshot(&contents));
    runtime_response(service_state, snapshot.as_ref())
}

fn probe_service_state() -> ServiceState {
    let Ok(mut child) = Command::new("/usr/bin/systemctl")
        .args([
            "--user",
            "show",
            "voice-input.service",
            "--property=ActiveState",
            "--value",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return ServiceState::Unknown;
    };

    let deadline = Instant::now() + SERVICE_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let mut output = Vec::new();
                return match child
                    .stdout
                    .take()
                    .and_then(|mut stdout| stdout.read_to_end(&mut output).ok())
                {
                    Some(_) => normalize_service_state(&output),
                    None => ServiceState::Unknown,
                };
            }
            Ok(Some(_)) | Err(_) => return ServiceState::Unknown,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return ServiceState::Unknown;
            }
        }
    }
}

fn normalize_service_state(output: &[u8]) -> ServiceState {
    match std::str::from_utf8(output).map(str::trim) {
        Ok("active") => ServiceState::Active,
        Ok("activating") | Ok("reloading") => ServiceState::Activating,
        Ok("deactivating") => ServiceState::Deactivating,
        Ok("inactive") => ServiceState::Inactive,
        Ok("failed") | Ok("maintenance") => ServiceState::Failed,
        _ => ServiceState::Unknown,
    }
}

fn parse_runtime_snapshot(contents: &[u8]) -> Option<Snapshot> {
    serde_json::from_slice(contents).ok()
}

fn runtime_response(service_state: ServiceState, snapshot: Option<&Snapshot>) -> Value {
    let runtime = match snapshot {
        Some(snapshot) => RuntimeStatus {
            available: true,
            phase: Some(snapshot.phase),
            updated_at_ms: Some(snapshot.updated_at_ms),
            language: nonempty(&snapshot.language),
            engine: nonempty(&snapshot.engine),
            model: nonempty(&snapshot.model),
        },
        None => RuntimeStatus {
            available: false,
            phase: None,
            updated_at_ms: None,
            language: None,
            engine: None,
            model: None,
        },
    };
    serde_json::to_value(RuntimeGetResult {
        service: ServiceStatus {
            state: service_state,
        },
        runtime,
    })
    .expect("runtime response serialization cannot fail")
}

fn nonempty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

fn settings_get(store: &ConfigStore) -> std::result::Result<Value, ProtocolError> {
    let loaded = store
        .load()
        .map_err(|_| error("config_read_failed", "configuration could not be loaded"))?;
    let credential_statuses = current_credential_statuses()?;
    Ok(json!({
        "config": loaded.config,
        "revision": loaded.revision,
        "credentials": credential_statuses,
        "choices": {
            "hotkey.mode": ["hold", "toggle"],
            "asr.provider": ["local-cli", "alibaba-qwen-realtime", "alibaba-qwen-audio3"],
            "asr.language": ["english", "simplified-chinese", "traditional-chinese", "japanese", "korean"],
            "asr.alibaba.turn_mode": ["server-vad", "manual"],
            "output.mode": ["type", "clipboard", "paste"],
            "hud.position": ["bottom-center", "bottom-left", "bottom-right"]
        }
    }))
}

fn settings_save(
    store: &ConfigStore,
    params: SaveParams,
) -> std::result::Result<Value, ProtocolError> {
    if contains_api_key(&params.config) {
        let mut fields = BTreeMap::new();
        fields.insert("config".into(), "must not contain api_key fields".into());
        return Err(ProtocolError {
            code: "validation_failed",
            message: "configuration validation failed".into(),
            fields: Some(fields),
        });
    }
    let config: Config = serde_json::from_value(params.config).map_err(|_| {
        error(
            "invalid_params",
            "config does not match the settings schema",
        )
    })?;
    config.validate().map_err(validation_error)?;

    let loaded = store
        .load()
        .map_err(|_| error("config_read_failed", "configuration could not be loaded"))?;
    if loaded.revision != params.revision {
        return Err(error(
            "revision_conflict",
            "configuration changed since it was loaded",
        ));
    }
    for (id, action) in &params.credentials {
        credentials::status(id)
            .map_err(|_| error("invalid_params", "unsupported credential ID"))?;
        match action.action.as_str() {
            "keep" if action.value.is_none() => {}
            "replace" => {
                let value = action
                    .value
                    .as_deref()
                    .ok_or_else(|| error("invalid_params", "replacement credential is required"))?;
                credentials::validate_replacement(id, value)
                    .map_err(|_| error("invalid_params", "invalid credential value"))?;
            }
            "keep" => return Err(error("invalid_params", "invalid credential action")),
            _ => return Err(error("invalid_params", "unknown credential action")),
        }
    }

    let legacy_values = BTreeMap::from([
        (ALIBABA_CREDENTIAL_ID, loaded.config.asr.alibaba.api_key),
        (OPENROUTER_CREDENTIAL_ID, loaded.config.llm.api_key),
    ]);
    let mut updated = BTreeMap::new();

    // Migrate a legacy plaintext value before serializing the config, because
    // Config deliberately omits api_key fields. A failed migration must not
    // silently remove the user's only copy of that credential.
    for id in [ALIBABA_CREDENTIAL_ID, OPENROUTER_CREDENTIAL_ID] {
        let action = params.credentials.get(id);
        let configured = credentials::status(id)
            .map_err(|_| {
                error(
                    "credential_status_failed",
                    "credential status could not be read",
                )
            })?
            .configured;
        let legacy = legacy_values.get(id).map(String::as_str).unwrap_or("");
        let replacement = action
            .filter(|action| action.action == "replace")
            .and_then(|action| action.value.as_deref());
        let value_to_protect =
            (!configured && !legacy.is_empty()).then_some(replacement.unwrap_or(legacy));
        if let Some(value) = value_to_protect {
            credentials::replace(id, value).map_err(|_| ProtocolError {
                code: "credential_write_failed",
                message: "legacy credential could not be migrated; configuration was not saved"
                    .into(),
                fields: Some(BTreeMap::from([(
                    format!("credentials.{id}"),
                    "credential could not be migrated".into(),
                )])),
            })?;
            updated.insert(id, true);
        } else {
            updated.insert(id, false);
        }
    }

    let revision = store
        .save(&config, Some(&params.revision))
        .map_err(store_error)?;

    let mut credential_errors = BTreeMap::new();
    for id in [ALIBABA_CREDENTIAL_ID, OPENROUTER_CREDENTIAL_ID] {
        if updated.get(id) == Some(&true) {
            continue;
        }
        let replacement = params.credentials.get(id).and_then(|action| {
            (action.action == "replace")
                .then_some(action.value.as_deref())
                .flatten()
        });
        if let Some(value) = replacement {
            match credentials::replace(id, value) {
                Ok(()) => {
                    updated.insert(id, true);
                }
                Err(_) => {
                    updated.insert(id, false);
                    credential_errors.insert(id, "credential could not be updated");
                }
            }
        }
    }

    let restart = if params.restart {
        match Command::new("/usr/bin/systemctl")
            .args(["--user", "restart", "voice-input.service"])
            .output()
        {
            Ok(output) if output.status.success() => json!({"requested": true, "ok": true}),
            _ => json!({
                "requested": true,
                "ok": false,
                "message": "voice-input.service could not be restarted"
            }),
        }
    } else {
        json!({"requested": false, "ok": true})
    };
    let restart_ok = restart["ok"].as_bool().unwrap_or(false);
    let has_credential_errors = !credential_errors.is_empty();
    let message = match (has_credential_errors, restart_ok) {
        (false, true) if params.restart => "Configuration saved; service restarted",
        (false, true) => "Configuration saved",
        (true, true) => "Configuration saved, but one or more credentials could not be updated",
        (false, false) => "Configuration saved, but the service could not be restarted",
        (true, false) => {
            "Configuration saved, but credential updates and the service restart had errors"
        }
    };
    let credential_statuses = current_credential_statuses().unwrap_or_else(|_| json!({}));

    Ok(json!({
        "saved": true,
        "config": config,
        "revision": revision,
        "credentials": credential_statuses,
        "credentials_updated": updated,
        "credential_errors": credential_errors,
        "restart": restart,
        "message": message,
        "partial": has_credential_errors || !restart_ok
    }))
}

fn current_credential_statuses() -> std::result::Result<Value, ProtocolError> {
    let alibaba = credentials::status(ALIBABA_CREDENTIAL_ID).map_err(|_| {
        error(
            "credential_status_failed",
            "credential status could not be read",
        )
    })?;
    let openrouter = credentials::status(OPENROUTER_CREDENTIAL_ID).map_err(|_| {
        error(
            "credential_status_failed",
            "credential status could not be read",
        )
    })?;
    Ok(json!({
        ALIBABA_CREDENTIAL_ID: alibaba,
        OPENROUTER_CREDENTIAL_ID: openrouter,
    }))
}

fn llm_test(
    store: &ConfigStore,
    params: LlmTestParams,
) -> std::result::Result<Value, ProtocolError> {
    let mut config = store
        .load()
        .map_err(|_| error("config_read_failed", "configuration could not be loaded"))?
        .config;
    config.llm = params.llm;
    config.llm.api_key = match params.credential.source.as_str() {
        "entered" => params
            .credential
            .value
            .filter(|value| !value.is_empty())
            .ok_or_else(|| error("invalid_params", "entered credential value is required"))?,
        "store" if params.credential.value.is_none() => {
            credentials::decrypt(OPENROUTER_CREDENTIAL_ID).map_err(|_| {
                error(
                    "credential_read_failed",
                    "stored credential could not be read",
                )
            })?
        }
        _ => return Err(error("invalid_params", "invalid credential source")),
    };
    config.validate().map_err(validation_error)?;
    llm::test_connectivity(&config)
        .map_err(|_| error("connectivity_failed", "LLM connectivity test failed"))?;
    Ok(json!({"connected": true}))
}

fn require_empty_object(params: &Value) -> std::result::Result<(), ProtocolError> {
    if params.as_object().is_some_and(serde_json::Map::is_empty) {
        Ok(())
    } else {
        Err(error("invalid_params", "params must be an empty object"))
    }
}

fn contains_api_key(value: &Value) -> bool {
    match value {
        Value::Object(values) => values
            .iter()
            .any(|(key, value)| key == "api_key" || contains_api_key(value)),
        Value::Array(values) => values.iter().any(contains_api_key),
        _ => false,
    }
}

fn validation_error(error: ValidationError) -> ProtocolError {
    ProtocolError {
        code: "validation_failed",
        message: "configuration validation failed".into(),
        fields: Some(error.fields),
    }
}

fn store_error(store_failure: anyhow::Error) -> ProtocolError {
    if store_failure.downcast_ref::<RevisionConflict>().is_some() {
        error(
            "revision_conflict",
            "configuration changed since it was loaded",
        )
    } else if let Some(validation) = store_failure.downcast_ref::<ValidationError>() {
        validation_error(validation.clone())
    } else {
        error("config_write_failed", "configuration could not be saved")
    }
}

fn error(code: &'static str, message: &'static str) -> ProtocolError {
    ProtocolError {
        code,
        message: message.into(),
        fields: None,
    }
}

fn serialize_error(id: i64, protocol_error: ProtocolError) -> Vec<u8> {
    serde_json::to_vec(&Response::<Value> {
        version: PROTOCOL_VERSION,
        id,
        ok: false,
        result: None,
        error: Some(protocol_error),
    })
    .expect("error serialization cannot fail")
}

fn write_error(writer: &mut impl Write, id: i64, protocol_error: ProtocolError) -> Result<()> {
    writer.write_all(&serialize_error(id, protocol_error))?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

enum LineError {
    TooLong,
    Io(io::Error),
}

fn read_line_limited(reader: &mut impl BufRead) -> std::result::Result<Option<Vec<u8>>, LineError> {
    let mut line = Vec::new();
    loop {
        let buffer = reader.fill_buf().map_err(LineError::Io)?;
        if buffer.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }
        if let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
            if line.len() + position > MAX_REQUEST_LINE {
                reader.consume(position + 1);
                return Err(LineError::TooLong);
            }
            line.extend_from_slice(&buffer[..position]);
            reader.consume(position + 1);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
        let length = buffer.len();
        if line.len() + length > MAX_REQUEST_LINE {
            reader.consume(length);
            drain_line(reader)?;
            return Err(LineError::TooLong);
        }
        line.extend_from_slice(buffer);
        reader.consume(length);
    }
}

fn drain_line(reader: &mut impl BufRead) -> std::result::Result<(), LineError> {
    loop {
        let buffer = reader.fill_buf().map_err(LineError::Io)?;
        if buffer.is_empty() {
            return Ok(());
        }
        let consumed = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|position| position + 1)
            .unwrap_or(buffer.len());
        let done = buffer.get(consumed - 1) == Some(&b'\n');
        reader.consume(consumed);
        if done {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        ServiceState, handle_line, normalize_service_state, parse_runtime_snapshot, run_with_io,
        runtime_response,
    };
    use crate::{
        config::{Config, ConfigStore},
        state::Snapshot,
    };

    #[test]
    fn service_state_is_bounded_and_normalized() {
        assert_eq!(normalize_service_state(b"active\n"), ServiceState::Active);
        assert_eq!(
            normalize_service_state(b"  inactive  \n"),
            ServiceState::Inactive
        );
        assert_eq!(
            normalize_service_state(b"activating\n"),
            ServiceState::Activating
        );
        assert_eq!(
            normalize_service_state(b"deactivating\n"),
            ServiceState::Deactivating
        );
        assert_eq!(normalize_service_state(b"failed\n"), ServiceState::Failed);
        assert_eq!(
            normalize_service_state(b"active but secret"),
            ServiceState::Unknown
        );
        assert_eq!(normalize_service_state(&[0xff]), ServiceState::Unknown);
    }

    #[test]
    fn missing_or_malformed_snapshot_is_unavailable_without_losing_service_state() {
        let missing = runtime_response(ServiceState::Inactive, None);
        assert_eq!(missing["service"]["state"], "inactive");
        assert_eq!(missing["runtime"], json!({"available": false}));

        let malformed = parse_runtime_snapshot(br#"{"phase":"recording"}"#);
        assert!(malformed.is_none());
        let response = runtime_response(ServiceState::Active, malformed.as_ref());
        assert_eq!(response["service"]["state"], "active");
        assert_eq!(response["runtime"], json!({"available": false}));
    }

    #[test]
    fn valid_snapshot_exposes_only_allowlisted_runtime_metadata() {
        let mut snapshot = Snapshot::idle(&Config::default());
        snapshot.language = "Japanese".into();
        snapshot.engine = "Safe Engine".into();
        snapshot.model = "Safe Model".into();
        snapshot.updated_at_ms = 1_234;
        snapshot.tooltip = "private tooltip".into();
        snapshot.transcript = "private transcript".into();
        snapshot.raw_transcript = Some("private raw transcript".into());
        snapshot.refined_transcript = Some("private refined transcript".into());
        snapshot.output_target_hint = Some("private output hint".into());
        snapshot.output_target_resolved = Some("private output target".into());
        snapshot.output_mode = Some("private output mode".into());
        snapshot.output_driver = Some("private output driver".into());
        snapshot.error = Some("private runtime error".into());

        let mut serialized = serde_json::to_value(snapshot).unwrap();
        serialized
            .as_object_mut()
            .unwrap()
            .insert("api_key".into(), json!("private credential"));
        let parsed = parse_runtime_snapshot(&serde_json::to_vec(&serialized).unwrap()).unwrap();
        let response = runtime_response(ServiceState::Active, Some(&parsed));

        assert_eq!(response["service"]["state"], "active");
        assert_eq!(response["runtime"]["available"], true);
        assert_eq!(response["runtime"]["phase"], "idle");
        assert_eq!(response["runtime"]["updated_at_ms"], 1_234);
        assert_eq!(response["runtime"]["language"], "Japanese");
        assert_eq!(response["runtime"]["engine"], "Safe Engine");
        assert_eq!(response["runtime"]["model"], "Safe Model");
        assert_eq!(response["runtime"].as_object().unwrap().len(), 6);

        let encoded = response.to_string();
        for forbidden in [
            "private tooltip",
            "private transcript",
            "private raw transcript",
            "private refined transcript",
            "private output hint",
            "private output target",
            "private output mode",
            "private output driver",
            "private runtime error",
            "private credential",
            "api_key",
            "tooltip",
            "transcript",
            "output_target",
            "error",
        ] {
            assert!(!encoded.contains(forbidden), "leaked {forbidden}");
        }
    }

    #[test]
    fn protocol_errors_are_framed_and_redacted() {
        let temp = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(temp.path().join("config.toml"));
        let secret = "do-not-echo-this-secret";
        let request = format!(
            "{{\"version\":1,\"id\":7,\"method\":\"unknown\",\"params\":{{\"api_key\":\"{secret}\"}}}}"
        );
        let response: Value =
            serde_json::from_slice(&handle_line(request.as_bytes(), &store)).unwrap();
        assert_eq!(response["version"], 1);
        assert_eq!(response["id"], 7);
        assert_eq!(response["ok"], false);
        assert!(!response.to_string().contains(secret));
    }

    #[test]
    fn settings_get_excludes_secrets_and_supplies_audio3_choices_and_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(temp.path().join("config.toml"));
        let request = json!({"version": 1, "id": 2, "method": "settings.get", "params": {}});
        let response: Value =
            serde_json::from_slice(&handle_line(request.to_string().as_bytes(), &store)).unwrap();
        assert_eq!(response["ok"], true);
        assert_eq!(response["result"]["config"]["audio"]["sample_rate"], 16_000);
        assert_eq!(
            response["result"]["choices"]["asr.provider"],
            json!(["local-cli", "alibaba-qwen-realtime", "alibaba-qwen-audio3"])
        );
        assert_eq!(
            response["result"]["config"]["asr"]["alibaba_audio3"],
            json!({
                "experimental_enabled": false,
                "endpoint": "wss://dashscope.aliyuncs.com/api-ws/v1/inference",
                "model": "qwen-audio-3.0-asr-flash-streaming",
                "language_hints_enabled": false,
                "heartbeat_enabled": false,
                "vocabulary": [],
                "native_endpoint": "https://dashscope.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation",
                "native_model": "qwen-audio-3.0-asr-flash",
                "native_final_pass_enabled": false,
                "native_timeout_ms": 20_000
            })
        );
        assert!(!response.to_string().contains("api_key"));
    }

    #[test]
    fn audio3_save_round_trip_requires_and_preserves_explicit_gate() {
        let temp = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(temp.path().join("config.toml"));
        let loaded = store.load().unwrap();
        let mut config = serde_json::to_value(loaded.config).unwrap();
        config["asr"]["provider"] = json!("alibaba-qwen-audio3");
        config["asr"]["alibaba_audio3"] = json!({
            "experimental_enabled": true,
            "endpoint": "wss://audio3.example.test/stream",
            "model": "audio3-stream-test",
            "language_hints_enabled": true,
            "heartbeat_enabled": true,
            "vocabulary": [
                {"term": "Voice Input", "weight": 5},
                {"term": "语音输入", "weight": 50}
            ],
            "native_endpoint": "https://audio3.example.test/native",
            "native_model": "audio3-native-test",
            "native_final_pass_enabled": true,
            "native_timeout_ms": 42_000
        });
        let request = json!({
            "version": 1,
            "id": 3,
            "method": "settings.save",
            "params": {
                "revision": loaded.revision,
                "config": config,
                "credentials": {},
                "restart": false
            }
        });
        let response: Value =
            serde_json::from_slice(&handle_line(request.to_string().as_bytes(), &store)).unwrap();

        assert_eq!(response["ok"], true);
        assert_eq!(
            response["result"]["config"]["asr"]["provider"],
            "alibaba-qwen-audio3"
        );
        assert_eq!(
            response["result"]["config"]["asr"]["alibaba_audio3"],
            config["asr"]["alibaba_audio3"]
        );
        let saved = store.load().unwrap().config;
        assert_eq!(
            saved.asr.provider,
            crate::config::AsrProvider::AlibabaQwenAudio3
        );
        assert!(saved.asr.alibaba_audio3.experimental_enabled);
        assert!(saved.asr.alibaba_audio3.language_hints_enabled);
        assert!(saved.asr.alibaba_audio3.heartbeat_enabled);
        assert_eq!(saved.asr.alibaba_audio3.vocabulary.len(), 2);
        assert!(saved.asr.alibaba_audio3.native_final_pass_enabled);
        assert_eq!(saved.asr.alibaba_audio3.native_timeout_ms, 42_000);
    }

    #[test]
    fn audio3_vocabulary_validation_response_never_echoes_private_terms() {
        const SENTINEL: &str = "private-settings-vocabulary-sentinel";
        let temp = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(temp.path().join("config.toml"));
        let loaded = store.load().unwrap();
        let mut config = serde_json::to_value(&loaded.config).unwrap();
        config["asr"]["provider"] = json!("alibaba-qwen-audio3");
        config["asr"]["alibaba_audio3"]["experimental_enabled"] = json!(true);
        config["asr"]["alibaba_audio3"]["vocabulary"] = json!([
            {"term": SENTINEL, "weight": 1},
            {"term": format!(" {SENTINEL} "), "weight": 2}
        ]);
        let request = json!({
            "version": 1,
            "id": 31,
            "method": "settings.save",
            "params": {
                "revision": loaded.revision,
                "config": config,
                "credentials": {},
                "restart": false
            }
        });
        let response: Value =
            serde_json::from_slice(&handle_line(request.to_string().as_bytes(), &store)).unwrap();

        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "validation_failed");
        assert_eq!(
            response["error"]["fields"]["asr.alibaba_audio3.vocabulary"],
            "entry 2 duplicates an earlier entry after trimming"
        );
        assert!(!response.to_string().contains(SENTINEL));
    }

    #[test]
    fn audio3_save_is_rejected_when_experimental_gate_is_false() {
        let temp = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(temp.path().join("config.toml"));
        let loaded = store.load().unwrap();
        let mut config = serde_json::to_value(loaded.config).unwrap();
        config["asr"]["provider"] = json!("alibaba-qwen-audio3");
        let request = json!({
            "version": 1,
            "id": 4,
            "method": "settings.save",
            "params": {
                "revision": loaded.revision,
                "config": config,
                "credentials": {},
                "restart": false
            }
        });
        let response: Value =
            serde_json::from_slice(&handle_line(request.to_string().as_bytes(), &store)).unwrap();

        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "validation_failed");
        assert_eq!(
            response["error"]["fields"]["asr.alibaba_audio3.experimental_enabled"],
            "must be true when the experimental provider is selected"
        );
        assert!(!store.path().exists());
    }

    #[test]
    fn save_round_trip_returns_current_revision_and_config() {
        let temp = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(temp.path().join("config.toml"));
        let loaded = store.load().unwrap();
        let request = json!({
            "version": 1,
            "id": 4,
            "method": "settings.save",
            "params": {
                "revision": loaded.revision,
                "config": loaded.config,
                "credentials": {},
                "restart": false
            }
        });
        let response: Value =
            serde_json::from_slice(&handle_line(request.to_string().as_bytes(), &store)).unwrap();
        assert_eq!(response["ok"], true);
        assert_eq!(response["result"]["saved"], true);
        assert_eq!(response["result"]["partial"], false);
        assert_eq!(response["result"]["config"]["audio"]["sample_rate"], 16_000);
        assert_eq!(
            response["result"]["revision"],
            store.load().unwrap().revision
        );
    }

    #[test]
    fn oversized_lines_receive_one_error_and_processing_continues() {
        let temp = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(temp.path().join("config.toml"));
        let mut input = vec![b'x'; 1024 * 1024 + 1];
        input.extend_from_slice(
            b"\n{\"version\":1,\"id\":9,\"method\":\"settings.get\",\"params\":{}}\n",
        );
        let mut output = Vec::new();
        run_with_io(input.as_slice(), &mut output, &store).unwrap();
        let lines: Vec<Value> = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["error"]["code"], "request_too_large");
        assert_eq!(lines[1]["id"], 9);
    }
}
