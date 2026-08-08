use std::io::{self, IsTerminal, Write};

use anyhow::{Context, bail};
use asterism_secrets::SecretString;
use zeroize::Zeroize;

pub fn read_secret(prompt: &str, maximum_bytes: usize) -> anyhow::Result<SecretString> {
    let mut value = if io::stdin().is_terminal() {
        rpassword::prompt_password(prompt).context("failed to read hidden terminal input")?
    } else {
        read_stdin_line()?
    };
    remove_line_ending(&mut value);
    if value.is_empty() || value.len() > maximum_bytes {
        value.zeroize();
        bail!("secret input is empty or exceeds its safety limit");
    }
    Ok(SecretString::new(value))
}

pub fn read_text(prompt: &str, maximum_bytes: usize) -> anyhow::Result<String> {
    if io::stdin().is_terminal() {
        let stdout = io::stdout();
        let mut output = stdout.lock();
        write!(output, "{prompt}").context("failed to write the terminal prompt")?;
        output
            .flush()
            .context("failed to flush the terminal prompt")?;
    }
    let mut value = read_stdin_line()?;
    remove_line_ending(&mut value);
    let value = value.trim().to_owned();
    if value.is_empty() || value.len() > maximum_bytes || value.chars().any(char::is_control) {
        bail!("text input is empty or exceeds its safety limit");
    }
    Ok(value)
}

fn read_stdin_line() -> anyhow::Result<String> {
    let mut value = String::new();
    io::stdin()
        .read_line(&mut value)
        .context("failed to read process input")?;
    Ok(value)
}

fn remove_line_ending(value: &mut String) {
    if value.ends_with('\n') {
        value.pop();
        if value.ends_with('\r') {
            value.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_endings_are_removed_without_trimming_secret_spaces() {
        let mut windows = " secret value \r\n".to_owned();
        remove_line_ending(&mut windows);
        assert_eq!(windows, " secret value ");

        let mut unix = "value\n".to_owned();
        remove_line_ending(&mut unix);
        assert_eq!(unix, "value");
    }
}
