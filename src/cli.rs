use anyhow::{bail, Result};

const BIN_NAME: &str = env!("CARGO_PKG_NAME");
const VERSION: &str = env!("CARGO_PKG_VERSION");
const DESCRIPTION: &str = env!("CARGO_PKG_DESCRIPTION");

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Run { headless: bool },
    Help,
    Version,
}

pub fn parse_args<I, S>(args: I) -> Result<Command>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut command = Command::Run { headless: false };
    let mut headless = false;
    for arg in args {
        match arg.as_ref() {
            "-h" | "--help" => command = Command::Help,
            "-V" | "--version" => command = Command::Version,
            "--headless" => headless = true,
            unknown => bail!("unknown argument '{unknown}'\n\n{}", help_text()),
        }
    }

    if matches!(command, Command::Run { .. }) {
        command = Command::Run { headless };
    }

    Ok(command)
}

pub fn version_text() -> String {
    format!("{BIN_NAME} {VERSION}")
}

pub fn help_text() -> String {
    format!(
        "{BIN_NAME} {VERSION}
{DESCRIPTION}

Usage:
  {BIN_NAME} [OPTIONS]

Options:
      --headless           Run without terminal UI, for local agents
  -h, --help               Print help
  -V, --version            Print version
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_help_and_version_flags() {
        assert_eq!(parse_args(["--help"]).unwrap(), Command::Help);
        assert_eq!(parse_args(["-h"]).unwrap(), Command::Help);
        assert_eq!(parse_args(["--version"]).unwrap(), Command::Version);
        assert_eq!(parse_args(["-V"]).unwrap(), Command::Version);
    }

    #[test]
    fn defaults_to_running_app_without_args() {
        assert_eq!(
            parse_args(std::iter::empty::<&str>()).unwrap(),
            Command::Run { headless: false }
        );
    }

    #[test]
    fn parses_headless_flag() {
        assert_eq!(
            parse_args(["--headless"]).unwrap(),
            Command::Run { headless: true }
        );
    }

    #[test]
    fn rejects_removed_api_flags() {
        let err = parse_args(["--api-socket", "/tmp/nope.sock"])
            .unwrap_err()
            .to_string();

        assert!(err.contains("unknown argument '--api-socket'"));
    }

    #[test]
    fn rejects_unknown_args() {
        let err = parse_args(["--wat"]).unwrap_err().to_string();

        assert!(err.contains("unknown argument '--wat'"));
        assert!(err.contains("Usage:"));
    }

    #[test]
    fn output_mentions_binary_name() {
        assert!(help_text().contains(BIN_NAME));
        assert!(version_text().starts_with(BIN_NAME));
    }
}
