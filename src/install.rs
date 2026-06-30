//! Interactive installation of the OpenDevTUI MCP server into coding-agent
//! configuration files (Claude Code, Codex, opencode).

use std::fmt;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

/// The coding agent whose configuration we are editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Claude,
    Codex,
    Opencode,
}

/// Where the configuration lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// User-wide configuration in the home directory.
    User,
    /// Repository-local configuration in the current workspace.
    Project,
}

/// Options gathered from the command line; missing fields are prompted for.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InstallArgs {
    pub tool: Option<Tool>,
    pub scope: Option<Scope>,
}

impl Tool {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_lowercase().as_str() {
            "claude" | "claude-code" => Ok(Tool::Claude),
            "codex" => Ok(Tool::Codex),
            "opencode" => Ok(Tool::Opencode),
            other => bail!("unknown tool '{other}' (expected claude, codex, or opencode)"),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tool::Claude => "Claude Code",
            Tool::Codex => "Codex",
            Tool::Opencode => "opencode",
        }
    }
}

impl fmt::Display for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl Scope {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_lowercase().as_str() {
            "user" | "global" => Ok(Scope::User),
            "project" | "repo" | "repository" | "local" => Ok(Scope::Project),
            other => bail!("unknown scope '{other}' (expected user or project)"),
        }
    }
}

/// The MCP server name written into every config file.
const SERVER_NAME: &str = "opendevtui";

/// Run the interactive (or argument-driven) installation flow.
pub fn run(args: InstallArgs) -> Result<()> {
    let tool = match args.tool {
        Some(tool) => tool,
        None => prompt_tool(&mut io::stdin().lock(), &mut io::stdout())?,
    };
    let scope = match args.scope {
        Some(scope) => scope,
        None => prompt_scope(&mut io::stdin().lock(), &mut io::stdout(), tool)?,
    };

    let workspace_root = std::env::current_dir()
        .and_then(|path| path.canonicalize())
        .unwrap_or_else(|_| PathBuf::from("."));
    let home = home_dir()?;
    let command = mcp_command();

    let path = config_path(tool, scope, &workspace_root, &home);
    let existing = match std::fs::read_to_string(&path) {
        Ok(contents) => Some(contents),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read {}", path.display()))
        }
    };

    let rendered = render_config(tool, &command, existing.as_deref())?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(&path, rendered)
        .with_context(|| format!("failed to write {}", path.display()))?;

    println!(
        "Installed the {SERVER_NAME} MCP server for {tool} ({}) at {}",
        scope_label(scope),
        path.display()
    );
    println!("Command: {command}");
    println!(
        "\nStart `opendevtui` in a workspace first; the MCP server connects to that project's socket."
    );
    Ok(())
}

fn scope_label(scope: Scope) -> &'static str {
    match scope {
        Scope::User => "user",
        Scope::Project => "project",
    }
}

/// Resolve the absolute path to the sibling `opendevtui-mcp` binary, falling
/// back to a bare command name resolved through `PATH`.
fn mcp_command() -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("opendevtui-mcp");
            if sibling.exists() {
                return sibling.to_string_lossy().into_owned();
            }
        }
    }
    "opendevtui-mcp".to_string()
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| anyhow!("could not determine home directory (HOME is not set)"))
}

/// Compute the configuration file path for the given tool and scope.
fn config_path(tool: Tool, scope: Scope, workspace_root: &Path, home: &Path) -> PathBuf {
    match (tool, scope) {
        (Tool::Claude, Scope::User) => home.join(".claude.json"),
        (Tool::Claude, Scope::Project) => workspace_root.join(".mcp.json"),
        (Tool::Codex, Scope::User) => home.join(".codex").join("config.toml"),
        (Tool::Codex, Scope::Project) => workspace_root.join(".codex").join("config.toml"),
        (Tool::Opencode, Scope::User) => home
            .join(".config")
            .join("opencode")
            .join("opencode.json"),
        (Tool::Opencode, Scope::Project) => workspace_root.join("opencode.json"),
    }
}

/// Merge the MCP server entry into existing configuration text (or create new
/// configuration) and return the serialized result to write back.
fn render_config(tool: Tool, command: &str, existing: Option<&str>) -> Result<String> {
    match tool {
        Tool::Claude => render_claude(command, existing),
        Tool::Opencode => render_opencode(command, existing),
        Tool::Codex => render_codex(command, existing),
    }
}

fn parse_json_object(existing: Option<&str>) -> Result<Map<String, Value>> {
    match existing {
        Some(contents) if !contents.trim().is_empty() => {
            let value: Value = serde_json::from_str(contents)
                .context("existing configuration is not valid JSON")?;
            match value {
                Value::Object(map) => Ok(map),
                _ => bail!("existing configuration is not a JSON object"),
            }
        }
        _ => Ok(Map::new()),
    }
}

/// Get a mutable reference to a nested object, creating it if absent and
/// erroring if the existing value is not an object.
fn object_entry<'a>(parent: &'a mut Map<String, Value>, key: &str) -> Result<&'a mut Map<String, Value>> {
    let entry = parent.entry(key.to_string()).or_insert_with(|| json!({}));
    entry
        .as_object_mut()
        .ok_or_else(|| anyhow!("existing `{key}` entry is not an object"))
}

fn render_claude(command: &str, existing: Option<&str>) -> Result<String> {
    let mut root = parse_json_object(existing)?;
    let servers = object_entry(&mut root, "mcpServers")?;
    servers.insert(
        SERVER_NAME.to_string(),
        json!({
            "type": "stdio",
            "command": command,
            "args": []
        }),
    );
    let mut out = serde_json::to_string_pretty(&Value::Object(root))?;
    out.push('\n');
    Ok(out)
}

fn render_opencode(command: &str, existing: Option<&str>) -> Result<String> {
    let mut root = parse_json_object(existing)?;
    root.entry("$schema".to_string())
        .or_insert_with(|| json!("https://opencode.ai/config.json"));
    let servers = object_entry(&mut root, "mcp")?;
    servers.insert(
        SERVER_NAME.to_string(),
        json!({
            "type": "local",
            "command": [command],
            "enabled": true
        }),
    );
    let mut out = serde_json::to_string_pretty(&Value::Object(root))?;
    out.push('\n');
    Ok(out)
}

fn render_codex(command: &str, existing: Option<&str>) -> Result<String> {
    let mut root: toml::Table = match existing {
        Some(contents) if !contents.trim().is_empty() => {
            contents.parse().context("existing configuration is not valid TOML")?
        }
        _ => toml::Table::new(),
    };

    let servers = match root
        .entry("mcp_servers".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
    {
        toml::Value::Table(table) => table,
        _ => bail!("existing `mcp_servers` entry is not a table"),
    };

    let mut entry = toml::Table::new();
    entry.insert("command".to_string(), toml::Value::String(command.to_string()));
    entry.insert("args".to_string(), toml::Value::Array(Vec::new()));
    servers.insert(SERVER_NAME.to_string(), toml::Value::Table(entry));

    let out = toml::to_string_pretty(&root).context("failed to serialize TOML configuration")?;
    Ok(out)
}

fn prompt_tool<R: BufRead, W: Write>(reader: &mut R, writer: &mut W) -> Result<Tool> {
    let options = [Tool::Claude, Tool::Codex, Tool::Opencode];
    writeln!(writer, "Which coding tool?")?;
    for (index, tool) in options.iter().enumerate() {
        writeln!(writer, "  {}) {}", index + 1, tool.label())?;
    }
    loop {
        write!(writer, "Enter choice [1-3]: ")?;
        writer.flush()?;
        let line = read_line(reader)?;
        let choice = line.trim();
        match choice {
            "1" => return Ok(Tool::Claude),
            "2" => return Ok(Tool::Codex),
            "3" => return Ok(Tool::Opencode),
            _ => {
                if let Ok(tool) = Tool::parse(choice) {
                    return Ok(tool);
                }
                writeln!(writer, "Please enter 1, 2, or 3.")?;
            }
        }
    }
}

fn prompt_scope<R: BufRead, W: Write>(reader: &mut R, writer: &mut W, tool: Tool) -> Result<Scope> {
    writeln!(writer, "\nInstall scope for {tool}?")?;
    writeln!(writer, "  1) User (all projects)")?;
    writeln!(writer, "  2) Project (this repository)")?;
    loop {
        write!(writer, "Enter choice [1-2]: ")?;
        writer.flush()?;
        let line = read_line(reader)?;
        let choice = line.trim();
        match choice {
            "1" => return Ok(Scope::User),
            "2" => return Ok(Scope::Project),
            _ => {
                if let Ok(scope) = Scope::parse(choice) {
                    return Ok(scope);
                }
                writeln!(writer, "Please enter 1 or 2.")?;
            }
        }
    }
}

fn read_line<R: BufRead>(reader: &mut R) -> Result<String> {
    let mut line = String::new();
    let bytes = reader.read_line(&mut line)?;
    if bytes == 0 {
        bail!("no input received");
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_aliases() {
        assert_eq!(Tool::parse("claude").unwrap(), Tool::Claude);
        assert_eq!(Tool::parse("Claude-Code").unwrap(), Tool::Claude);
        assert_eq!(Tool::parse("CODEX").unwrap(), Tool::Codex);
        assert_eq!(Tool::parse("opencode").unwrap(), Tool::Opencode);
        assert!(Tool::parse("vim").is_err());
    }

    #[test]
    fn parses_scope_aliases() {
        assert_eq!(Scope::parse("user").unwrap(), Scope::User);
        assert_eq!(Scope::parse("global").unwrap(), Scope::User);
        assert_eq!(Scope::parse("repo").unwrap(), Scope::Project);
        assert_eq!(Scope::parse("project").unwrap(), Scope::Project);
        assert!(Scope::parse("nowhere").is_err());
    }

    #[test]
    fn config_paths_match_each_tool() {
        let ws = PathBuf::from("/ws");
        let home = PathBuf::from("/home/me");
        assert_eq!(
            config_path(Tool::Claude, Scope::User, &ws, &home),
            home.join(".claude.json")
        );
        assert_eq!(
            config_path(Tool::Claude, Scope::Project, &ws, &home),
            ws.join(".mcp.json")
        );
        assert_eq!(
            config_path(Tool::Codex, Scope::User, &ws, &home),
            home.join(".codex/config.toml")
        );
        assert_eq!(
            config_path(Tool::Opencode, Scope::User, &ws, &home),
            home.join(".config/opencode/opencode.json")
        );
        assert_eq!(
            config_path(Tool::Opencode, Scope::Project, &ws, &home),
            ws.join("opencode.json")
        );
    }

    #[test]
    fn claude_config_created_from_scratch() {
        let out = render_claude("/bin/opendevtui-mcp", None).unwrap();
        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["mcpServers"][SERVER_NAME]["command"], "/bin/opendevtui-mcp");
        assert_eq!(value["mcpServers"][SERVER_NAME]["type"], "stdio");
    }

    #[test]
    fn claude_config_preserves_existing_servers() {
        let existing = r#"{"numStartups": 3, "mcpServers": {"other": {"command": "x"}}}"#;
        let out = render_claude("/bin/opendevtui-mcp", Some(existing)).unwrap();
        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["numStartups"], 3);
        assert_eq!(value["mcpServers"]["other"]["command"], "x");
        assert_eq!(value["mcpServers"][SERVER_NAME]["command"], "/bin/opendevtui-mcp");
    }

    #[test]
    fn opencode_config_sets_schema_and_local_command() {
        let out = render_opencode("/bin/opendevtui-mcp", None).unwrap();
        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["$schema"], "https://opencode.ai/config.json");
        assert_eq!(value["mcp"][SERVER_NAME]["type"], "local");
        assert_eq!(value["mcp"][SERVER_NAME]["command"][0], "/bin/opendevtui-mcp");
        assert_eq!(value["mcp"][SERVER_NAME]["enabled"], true);
    }

    #[test]
    fn codex_config_preserves_existing_keys() {
        let existing = "model = \"o3\"\n\n[mcp_servers.other]\ncommand = \"x\"\n";
        let out = render_codex("/bin/opendevtui-mcp", Some(existing)).unwrap();
        let value: toml::Table = out.parse().unwrap();
        assert_eq!(value["model"].as_str(), Some("o3"));
        assert_eq!(
            value["mcp_servers"]["other"]["command"].as_str(),
            Some("x")
        );
        assert_eq!(
            value["mcp_servers"][SERVER_NAME]["command"].as_str(),
            Some("/bin/opendevtui-mcp")
        );
    }

    #[test]
    fn rejects_non_object_json() {
        assert!(render_claude("/bin/x", Some("[]")).is_err());
    }

    #[test]
    fn prompt_tool_accepts_numeric_choice() {
        let mut input = io::Cursor::new(b"2\n".to_vec());
        let mut output = Vec::new();
        let tool = prompt_tool(&mut input, &mut output).unwrap();
        assert_eq!(tool, Tool::Codex);
    }

    #[test]
    fn prompt_scope_reprompts_on_invalid() {
        let mut input = io::Cursor::new(b"x\n2\n".to_vec());
        let mut output = Vec::new();
        let scope = prompt_scope(&mut input, &mut output, Tool::Claude).unwrap();
        assert_eq!(scope, Scope::Project);
    }
}
