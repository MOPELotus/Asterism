use std::io::{self, BufRead, IsTerminal};

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

pub fn read_credential_values(prompts: &[&str]) -> anyhow::Result<Vec<SecretString>> {
    if prompts.is_empty() {
        bail!("at least one credential purpose is required");
    }

    if io::stdin().is_terminal() {
        prompts
            .iter()
            .map(|prompt| {
                let value = rpassword::prompt_password(format!("{prompt}: "))
                    .with_context(|| format!("failed to read {prompt} from terminal"))?;
                validate_credential_value(value)
            })
            .collect()
    } else {
        let stdin = io::stdin();
        read_credential_lines(stdin.lock(), prompts.len())
    }
}

fn read_credential_lines(
    mut reader: impl BufRead,
    count: usize,
) -> anyhow::Result<Vec<SecretString>> {
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let mut value = String::new();
        reader
            .read_line(&mut value)
            .with_context(|| format!("failed to read credential {} from stdin", index + 1))?;
        trim_line_ending(&mut value);
        values.push(
            validate_credential_value(value)
                .with_context(|| format!("credential {} read from stdin is invalid", index + 1))?,
        );
    }
    Ok(values)
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

    #[test]
    fn multi_credential_reader_preserves_line_order_and_spaces() {
        let values = read_credential_lines("student@example.com\npass phrase\r\n".as_bytes(), 2)
            .expect("two credential lines should be accepted");

        assert_eq!(values.len(), 2);
        assert_eq!(values[0].expose_secret(), "student@example.com");
        assert_eq!(values[1].expose_secret(), "pass phrase");
    }

    #[test]
    fn multi_credential_reader_requires_every_requested_line() {
        assert!(read_credential_lines("only-one\n".as_bytes(), 2).is_err());
    }
}
