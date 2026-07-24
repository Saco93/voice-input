use std::{env, fs, path::PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::Config;

pub const ALIBABA_CREDENTIAL_ID: &str = "alibaba-api-key";
pub const OPENROUTER_CREDENTIAL_ID: &str = "openrouter-api-key";

pub fn apply_runtime_credentials(config: &mut Config) -> Result<()> {
    config.asr.alibaba.api_key = resolve(
        ALIBABA_CREDENTIAL_ID,
        "VOICE_INPUT_ALIBABA_API_KEY",
        &config.asr.alibaba.api_key,
    )?;
    config.llm.api_key = resolve(
        OPENROUTER_CREDENTIAL_ID,
        "VOICE_INPUT_OPENROUTER_API_KEY",
        &config.llm.api_key,
    )?;
    Ok(())
}

fn resolve(credential_id: &str, environment_name: &str, legacy: &str) -> Result<String> {
    if let Some(directory) = env::var_os("CREDENTIALS_DIRECTORY") {
        let path = PathBuf::from(directory).join(credential_id);
        if path.exists() {
            let value = fs::read_to_string(&path)
                .with_context(|| format!("failed to read systemd credential `{credential_id}`"))?;
            return validate(credential_id, value);
        }
    }

    if let Ok(value) = env::var(environment_name)
        && !value.trim().is_empty()
    {
        return validate(credential_id, value);
    }

    validate(credential_id, legacy.to_string())
}

fn validate(credential_id: &str, value: String) -> Result<String> {
    let value = value.trim_end_matches(['\r', '\n']).to_string();
    if value.contains('\0') {
        bail!("credential `{credential_id}` contains an invalid NUL byte");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::validate;

    #[test]
    fn trims_only_trailing_newlines() {
        assert_eq!(
            validate("test", "  secret  \n".into()).unwrap(),
            "  secret  "
        );
    }

    #[test]
    fn rejects_nul_bytes() {
        assert!(validate("test", "bad\0secret".into()).is_err());
    }
}
