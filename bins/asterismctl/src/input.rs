use std::io::{self, IsTerminal};

use anyhow::{Context, bail};
use asterism_secrets::SecretString;
use zeroize::Zeroize;

const SERVICE_TOKEN_ENV: &str = "ASTERISM_TOKEN";
const MAX_CREDENTIAL_VALUE_BYTES: usize = 12 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PasswordMode {
    Stdin,
    Terminal { confirm: bool },
}

pub fn read_password(mode: PasswordMode) -> anyhow::Result<SecretString> {
    match mode {
        PasswordMode::Stdin => read_password_line(),
        PasswordMode::Terminal { confirm } => {
            let password = SecretString::new(
                rpassword::prompt_password("Password: ")
                    .context("failed to read password from terminal")?,
            );
            if confirm {
                let confirmation = SecretString::new(
                    rpassword::prompt_password("Confirm password: ")
                        .context("failed to confirm password from terminal")?,
                );
                if password.expose_secret() != confirmation.expose_secret() {
                    bail!("password confirmation does not match");
                }
            }
            Ok(password)
        }
    }
}

pub fn service_token_from_process() -> anyhow::Result<SecretString> {
    let value = std::env::var_os(SERVICE_TOKEN_ENV)
        .context("ASTERISM_TOKEN is required for this command")?
        .into_string()
        .map_err(|_| anyhow::anyhow!("ASTERISM_TOKEN is not valid Unicode"))?;
    validate_service_token(value)
}

pub fn read_credential_value() -> anyhow::Result<SecretString> {
    if io::stdin().is_terminal() {
        let value = rpassword::prompt_password("Credential value: ")
            .context("failed to read credential from terminal")?;
        validate_credential_value(value)
    } else {
        let mut value = String::new();
        io::stdin()
            .read_line(&mut value)
            .context("failed to read credential from stdin")?;
        trim_line_ending(&mut value);
        validate_credential_value(value)
    }
}

fn read_password_line() -> anyhow::Result<SecretString> {
    let mut value = String::new();
    io::stdin()
        .read_line(&mut value)
        .context("failed to read password from stdin")?;
    trim_line_ending(&mut value);
    if value.is_empty() {
        value.zeroize();
        bail!("password read from stdin is empty");
    }
    Ok(SecretString::new(value))
}

fn validate_credential_value(mut value: String) -> anyhow::Result<SecretString> {
    let valid = !value.is_empty()
        && value.len() <= MAX_CREDENTIAL_VALUE_BYTES
        && !value.chars().any(char::is_control);
    if !valid {
        value.zeroize();
        bail!(
            "credential must contain 1-{MAX_CREDENTIAL_VALUE_BYTES} bytes without control characters"
        );
    }
    Ok(SecretString::new(value))
}

fn trim_line_ending(value: &mut String) {
    while matches!(value.as_bytes().last(), Some(b'\n' | b'\r')) {
        value.pop();
    }
}

fn validate_service_token(mut value: String) -> anyhow::Result<SecretString> {
    let valid = value.starts_with("ast_st_")
        && value.len() <= 512
        && !value.chars().any(char::is_whitespace)
        && !value.chars().any(char::is_control);
    if !valid {
        value.zeroize();
        bail!("ASTERISM_TOKEN does not contain a valid Asterism service token");
    }
    Ok(SecretString::new(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_validation_accepts_only_service_token_shape() {
        assert!(validate_service_token("ast_st_example".to_owned()).is_ok());
        assert!(validate_service_token("ast_ws_example".to_owned()).is_err());
        assert!(validate_service_token("ast_st_has whitespace".to_owned()).is_err());
    }

    #[test]
    fn credential_validation_rejects_empty_control_or_oversized_values() {
        assert!(validate_credential_value("cookie-value".to_owned()).is_ok());
        assert!(validate_credential_value(String::new()).is_err());
        assert!(validate_credential_value("line\nbreak".to_owned()).is_err());
        assert!(validate_credential_value("x".repeat(MAX_CREDENTIAL_VALUE_BYTES + 1)).is_err());
    }
}
