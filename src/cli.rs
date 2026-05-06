use anyhow::{bail, Result};

const BIN_NAME: &str = env!("CARGO_PKG_NAME");
const VERSION: &str = env!("CARGO_PKG_VERSION");
const DESCRIPTION: &str = env!("CARGO_PKG_DESCRIPTION");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Run,
    Help,
    Version,
}

pub fn parse_args<I, S>(args: I) -> Result<Command>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut command = Command::Run;

    for arg in args {
        match arg.as_ref() {
            "-h" | "--help" => command = Command::Help,
            "-V" | "--version" => command = Command::Version,
            unknown => bail!("unknown argument '{unknown}'\n\n{}", help_text()),
        }
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
  -h, --help       Print help
  -V, --version    Print version
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
            Command::Run
        );
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
