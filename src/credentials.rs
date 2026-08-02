use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use tempfile::Builder;

use crate::config::Config;

pub const ALIBABA_CREDENTIAL_ID: &str = "alibaba-api-key";
pub const OPENROUTER_CREDENTIAL_ID: &str = "openrouter-api-key";
const MAX_CREDENTIAL_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct CredentialStatus {
    pub id: &'static str,
    pub configured: bool,
}

pub fn apply_runtime_credentials(config: &mut Config) -> Result<()> {
    let alibaba_api_key = resolve(
        ALIBABA_CREDENTIAL_ID,
        "VOICE_INPUT_ALIBABA_API_KEY",
        &config.asr.alibaba.api_key,
    )?;
    config.asr.alibaba.api_key = alibaba_api_key.clone();
    config.asr.alibaba_audio3.api_key = alibaba_api_key;
    config.llm.api_key = resolve(
        OPENROUTER_CREDENTIAL_ID,
        "VOICE_INPUT_OPENROUTER_API_KEY",
        &config.llm.api_key,
    )?;
    Ok(())
}

pub fn status(credential_id: &str) -> Result<CredentialStatus> {
    validate_id(credential_id)?;
    Ok(CredentialStatus {
        id: fixed_id(credential_id)?,
        configured: credential_path(credential_id)?.is_file(),
    })
}

pub fn credential_path(credential_id: &str) -> Result<PathBuf> {
    validate_id(credential_id)?;
    Ok(credstore_dir()?.join(credential_id))
}

pub fn decrypt(credential_id: &str) -> Result<String> {
    decrypt_with(&SystemdCreds, credential_id)
}

pub fn replace(credential_id: &str, value: &str) -> Result<()> {
    replace_with(&SystemdCreds, credential_id, value)
}

pub fn validate_replacement(credential_id: &str, value: &str) -> Result<()> {
    validate_value(credential_id, value.to_string(), false).map(|_| ())
}

fn credstore_dir() -> Result<PathBuf> {
    let config_home =
        dirs::config_dir().ok_or_else(|| anyhow::anyhow!("XDG config directory not found"))?;
    Ok(config_home.join("credstore.encrypted"))
}

fn resolve(credential_id: &str, environment_name: &str, legacy: &str) -> Result<String> {
    if let Some(directory) = env::var_os("CREDENTIALS_DIRECTORY") {
        let path = PathBuf::from(directory).join(credential_id);
        if path.exists() {
            let value = fs::read_to_string(&path)
                .with_context(|| format!("failed to read systemd credential `{credential_id}`"))?;
            return validate_value(credential_id, value, true);
        }
    }

    if let Ok(value) = env::var(environment_name)
        && !value.trim().is_empty()
    {
        return validate_value(credential_id, value, true);
    }

    validate_value(credential_id, legacy.to_string(), true)
}

fn validate_id(credential_id: &str) -> Result<()> {
    fixed_id(credential_id).map(|_| ())
}

fn fixed_id(credential_id: &str) -> Result<&'static str> {
    match credential_id {
        ALIBABA_CREDENTIAL_ID => Ok(ALIBABA_CREDENTIAL_ID),
        OPENROUTER_CREDENTIAL_ID => Ok(OPENROUTER_CREDENTIAL_ID),
        _ => bail!("unsupported credential ID"),
    }
}

fn validate_value(credential_id: &str, value: String, allow_empty: bool) -> Result<String> {
    validate_id(credential_id)?;
    let value = value.trim_end_matches(['\r', '\n']).to_string();
    if !allow_empty && value.is_empty() {
        bail!("credential value must not be empty");
    }
    if value.len() > MAX_CREDENTIAL_BYTES {
        bail!("credential value is too long");
    }
    if value.contains('\0') || value.chars().any(|character| character.is_control()) {
        bail!("credential value contains invalid control characters");
    }
    Ok(value)
}

struct CommandOutput {
    success: bool,
    stdout: Vec<u8>,
}

trait CredentialCommand {
    fn run(&self, arguments: &[String], stdin: &[u8]) -> Result<CommandOutput>;
}

struct SystemdCreds;

impl CredentialCommand for SystemdCreds {
    fn run(&self, arguments: &[String], stdin: &[u8]) -> Result<CommandOutput> {
        let mut child = Command::new("systemd-creds")
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to start systemd-creds")?;
        child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("systemd-creds stdin was unavailable"))?
            .write_all(stdin)
            .context("failed to provide credential to systemd-creds")?;
        let output = child
            .wait_with_output()
            .context("failed to wait for systemd-creds")?;
        Ok(CommandOutput {
            success: output.status.success(),
            stdout: output.stdout,
        })
    }
}

fn decrypt_with(runner: &dyn CredentialCommand, credential_id: &str) -> Result<String> {
    decrypt_at_with(runner, credential_id, &credential_path(credential_id)?)
}

fn decrypt_at_with(
    runner: &dyn CredentialCommand,
    credential_id: &str,
    path: &Path,
) -> Result<String> {
    validate_id(credential_id)?;
    if !path.is_file() {
        bail!("credential is not configured");
    }
    let arguments = vec![
        "decrypt".into(),
        "--user".into(),
        format!("--name={credential_id}"),
        path.to_string_lossy().into_owned(),
        "-".into(),
    ];
    let output = runner.run(&arguments, &[])?;
    if !output.success {
        bail!("systemd-creds could not decrypt the credential");
    }
    let value = String::from_utf8(output.stdout).context("decrypted credential is not UTF-8")?;
    validate_value(credential_id, value, false)
}

fn replace_with(runner: &dyn CredentialCommand, credential_id: &str, value: &str) -> Result<()> {
    replace_at_with(
        runner,
        credential_id,
        value,
        &credential_path(credential_id)?,
    )
}

fn replace_at_with(
    runner: &dyn CredentialCommand,
    credential_id: &str,
    value: &str,
    destination: &Path,
) -> Result<()> {
    let value = validate_value(credential_id, value.to_string(), false)?;
    let directory = destination
        .parent()
        .ok_or_else(|| anyhow::anyhow!("credential path has no parent directory"))?;
    make_private_directory(directory)?;
    let temporary = Builder::new()
        .prefix(&format!(".{credential_id}."))
        .tempfile_in(directory)
        .context("failed to create temporary credential file")?;
    let (file, temporary_path) = temporary
        .keep()
        .context("failed to reserve credential path")?;
    drop(file);
    fs::remove_file(&temporary_path).context("failed to prepare temporary credential path")?;

    let arguments = vec![
        "encrypt".into(),
        "--user".into(),
        format!("--name={credential_id}"),
        "-".into(),
        temporary_path.to_string_lossy().into_owned(),
    ];
    let mut input = value.into_bytes();
    input.push(b'\n');
    let output = runner.run(&arguments, &input)?;
    input.fill(0);
    if !output.success {
        let _ = fs::remove_file(&temporary_path);
        bail!("systemd-creds could not encrypt the credential");
    }
    if !temporary_path.is_file() {
        bail!("systemd-creds did not create an encrypted credential");
    }
    set_private_path(&temporary_path)?;
    fs::rename(&temporary_path, destination).context("failed to replace encrypted credential")?;
    set_private_path(destination)?;
    Ok(())
}

fn make_private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to secure {}", path.display()))?;
    }
    Ok(())
}

fn set_private_path(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to secure {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::{
        ALIBABA_CREDENTIAL_ID, CommandOutput, CredentialCommand, decrypt_at_with, replace_at_with,
        validate_value,
    };

    struct FakeCommand {
        calls: Mutex<Vec<(Vec<String>, Vec<u8>)>>,
        output: Mutex<Option<CommandOutput>>,
    }

    impl CredentialCommand for FakeCommand {
        fn run(&self, arguments: &[String], stdin: &[u8]) -> anyhow::Result<CommandOutput> {
            self.calls
                .lock()
                .unwrap()
                .push((arguments.to_vec(), stdin.to_vec()));
            Ok(self.output.lock().unwrap().take().unwrap())
        }
    }

    #[test]
    fn validates_fixed_ids_and_values() {
        assert!(validate_value("arbitrary", "secret".into(), false).is_err());
        assert!(validate_value(ALIBABA_CREDENTIAL_ID, "".into(), false).is_err());
        assert!(validate_value(ALIBABA_CREDENTIAL_ID, "bad\0secret".into(), false).is_err());
        assert_eq!(
            validate_value(ALIBABA_CREDENTIAL_ID, "  secret  \n".into(), false).unwrap(),
            "  secret  "
        );
    }

    #[test]
    fn decrypt_failure_is_sanitized() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("encrypted");
        std::fs::write(&path, "ciphertext").unwrap();
        let runner = FakeCommand {
            calls: Mutex::new(Vec::new()),
            output: Mutex::new(Some(CommandOutput {
                success: false,
                stdout: b"provider-secret".to_vec(),
            })),
        };
        let error = decrypt_at_with(&runner, ALIBABA_CREDENTIAL_ID, &path).unwrap_err();
        assert!(!error.to_string().contains("provider-secret"));
    }

    #[test]
    fn replacement_secret_is_stdin_only() {
        let temp = tempfile::tempdir().unwrap();
        let runner = FakeCommand {
            calls: Mutex::new(Vec::new()),
            output: Mutex::new(Some(CommandOutput {
                success: false,
                stdout: Vec::new(),
            })),
        };
        let secret = "credential-value-not-for-argv";
        let destination = temp.path().join("credstore.encrypted/alibaba-api-key");
        let _ = replace_at_with(&runner, ALIBABA_CREDENTIAL_ID, secret, &destination);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(!calls[0].0.iter().any(|argument| argument.contains(secret)));
        assert_eq!(calls[0].1, format!("{secret}\n").as_bytes());
    }
}
