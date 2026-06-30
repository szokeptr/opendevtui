use anyhow::{bail, Result};

use crate::install::{InstallArgs, Scope, Tool};

const BIN_NAME: &str = env!("CARGO_PKG_NAME");
const VERSION: &str = env!("CARGO_PKG_VERSION");
const DESCRIPTION: &str = env!("CARGO_PKG_DESCRIPTION");

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Run { headless: bool },
    McpInstall(InstallArgs),
    Help,
    Version,
}

pub fn parse_args<I, S>(args: I) -> Result<Command>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args.into_iter().map(|a| a.as_ref().to_string()).collect();

    if let Some(first) = args.first() {
        if first == "mcp" {
            return parse_mcp(&args[1..]);
        }
    }

    let mut command = Command::Run { headless: false };
    let mut headless = false;
    for arg in &args {
        match arg.as_str() {
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

fn parse_mcp(args: &[String]) -> Result<Command> {
    let Some(subcommand) = args.first() else {
        bail!("missing 'mcp' subcommand\n\n{}", help_text());
    };

    match subcommand.as_str() {
        "-h" | "--help" => return Ok(Command::Help),
        "install" => {}
        other => bail!("unknown 'mcp' subcommand '{other}'\n\n{}", help_text()),
    }

    let mut install = InstallArgs::default();
    let mut rest = args[1..].iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--tool" => {
                let value = rest
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--tool requires a value"))?;
                install.tool = Some(Tool::parse(value)?);
            }
            "--scope" => {
                let value = rest
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--scope requires a value"))?;
                install.scope = Some(Scope::parse(value)?);
            }
            unknown => bail!("unknown argument '{unknown}'\n\n{}", help_text()),
        }
    }

    Ok(Command::McpInstall(install))
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
  {BIN_NAME} mcp install [--tool <claude|codex|opencode>] [--scope <user|project>]

Commands:
  mcp install              Install the MCP server into a coding agent's config
                           (interactive when --tool/--scope are omitted)

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
    fn parses_mcp_install_without_options() {
        assert_eq!(
            parse_args(["mcp", "install"]).unwrap(),
            Command::McpInstall(InstallArgs::default())
        );
    }

    #[test]
    fn parses_mcp_install_with_tool_and_scope() {
        assert_eq!(
            parse_args(["mcp", "install", "--tool", "codex", "--scope", "user"]).unwrap(),
            Command::McpInstall(InstallArgs {
                tool: Some(Tool::Codex),
                scope: Some(Scope::User),
            })
        );
    }

    #[test]
    fn rejects_unknown_mcp_subcommand() {
        let err = parse_args(["mcp", "frobnicate"]).unwrap_err().to_string();
        assert!(err.contains("unknown 'mcp' subcommand 'frobnicate'"));
    }

    #[test]
    fn output_mentions_binary_name() {
        assert!(help_text().contains(BIN_NAME));
        assert!(version_text().starts_with(BIN_NAME));
    }
}
