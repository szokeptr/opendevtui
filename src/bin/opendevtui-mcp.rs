#[cfg(not(unix))]
compile_error!("opendevtui MCP server currently supports Unix only");

use std::io::{self, Read, Write};
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug)]
struct Server {
    workspace_root: PathBuf,
    socket_path: PathBuf,
}

#[derive(Debug, Serialize)]
struct InstanceSnapshot {
    workspace_root: PathBuf,
    socket_path: PathBuf,
    running: bool,
}

fn main() -> Result<()> {
    trace("starting");
    let workspace_root = std::env::current_dir()
        .context("failed to read MCP server current directory")?
        .canonicalize()
        .context("failed to canonicalize MCP server current directory")?;
    let socket_path = opendevtui::api::project_socket_path(&workspace_root);
    let server = Server {
        workspace_root,
        socket_path,
    };
    let mut reader = MessageReader::new(io::stdin());
    let mut writer = io::stdout();

    while let Some(message) = reader.read_message()? {
        let framing = reader.framing();
        trace(&format!(
            "received {}",
            message
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("<response>")
        ));
        let Some(response) = server.handle(message) else {
            continue;
        };
        write_message(&mut writer, &response, framing)?;
    }

    Ok(())
}

impl Server {
    fn handle(&self, message: Value) -> Option<Value> {
        let id = message.get("id").cloned();
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");

        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": "2025-06-18",
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "opendevtui",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "instructions": "Controls the already-running OpenDevTUI instance for this workspace through its project-scoped Unix socket. Start OpenDevTUI in the project before using these tools."
            })),
            "notifications/initialized" => return None,
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => self.call_tool(message.get("params").unwrap_or(&Value::Null)),
            _ => Err(anyhow!("unsupported method `{method}`")),
        };

        id.map(|id| match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(err) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32000, "message": err.to_string() }
            }),
        })
    }

    fn call_tool(&self, params: &Value) -> Result<Value> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .context("tools/call missing params.name")?;
        let args = params.get("arguments").unwrap_or(&Value::Null);

        let payload = match name {
            "opendevtui_status" => json!(self.status()),
            "opendevtui_list_services" => self.request_json("GET", "/services", None)?,
            "opendevtui_start_service" => {
                let id = required_string_arg(args, "service_id")?;
                self.request_json("POST", &format!("/services/{id}/start"), None)?
            }
            "opendevtui_stop_service" => {
                let id = required_string_arg(args, "service_id")?;
                self.request_json("POST", &format!("/services/{id}/stop"), None)?
            }
            "opendevtui_restart_service" => {
                let id = required_string_arg(args, "service_id")?;
                self.request_json("POST", &format!("/services/{id}/restart"), None)?
            }
            "opendevtui_get_logs" => {
                let id = required_string_arg(args, "service_id")?;
                let tail = args.get("tail").and_then(Value::as_u64);
                let path = tail
                    .map(|tail| format!("/services/{id}/logs?tail={tail}"))
                    .unwrap_or_else(|| format!("/services/{id}/logs"));
                self.request_json("GET", &path, None)?
            }
            "opendevtui_clear_logs" => {
                let id = required_string_arg(args, "service_id")?;
                self.request_json("POST", &format!("/services/{id}/logs/clear"), None)?
            }
            unknown => bail!("unknown tool `{unknown}`"),
        };

        Ok(json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&payload)?
            }]
        }))
    }

    fn status(&self) -> InstanceSnapshot {
        InstanceSnapshot {
            workspace_root: self.workspace_root.clone(),
            socket_path: self.socket_path.clone(),
            running: self.socket_path.exists(),
        }
    }

    fn request_json(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let response = self.request(method, path, body)?;
        parse_http_json_response(&response)
    }

    fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Vec<u8>> {
        use std::os::unix::net::UnixStream;

        let mut stream = UnixStream::connect(&self.socket_path).with_context(|| {
            format!(
                "failed to connect to {}; start opendevtui in {} first",
                self.socket_path.display(),
                self.workspace_root.display()
            )
        })?;
        let body = match body {
            Some(value) => serde_json::to_vec(value).context("failed to serialize request body")?,
            None => Vec::new(),
        };
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: opendevtui.local\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .context("failed to write request header")?;
        stream
            .write_all(&body)
            .context("failed to write request body")?;

        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .context("failed to read response")?;
        Ok(response)
    }
}

fn tools() -> Value {
    json!([
        {
            "name": "opendevtui_status",
            "description": "Show the project-scoped OpenDevTUI socket path and whether it exists.",
            "inputSchema": object_schema(json!({}))
        },
        {
            "name": "opendevtui_list_services",
            "description": "List services from the already-running OpenDevTUI instance for this project.",
            "inputSchema": object_schema(json!({}))
        },
        {
            "name": "opendevtui_start_service",
            "description": "Start one service in the already-running OpenDevTUI instance.",
            "inputSchema": object_schema(json!({
                "service_id": {
                    "type": "string",
                    "description": "Service id from opendevtui_list_services."
                }
            }))
        },
        {
            "name": "opendevtui_stop_service",
            "description": "Stop one service in the already-running OpenDevTUI instance.",
            "inputSchema": object_schema(json!({
                "service_id": {
                    "type": "string",
                    "description": "Service id from opendevtui_list_services."
                }
            }))
        },
        {
            "name": "opendevtui_restart_service",
            "description": "Restart one service in the already-running OpenDevTUI instance.",
            "inputSchema": object_schema(json!({
                "service_id": {
                    "type": "string",
                    "description": "Service id from opendevtui_list_services."
                }
            }))
        },
        {
            "name": "opendevtui_get_logs",
            "description": "Get logs for one service in the already-running OpenDevTUI instance.",
            "inputSchema": object_schema(json!({
                "service_id": {
                    "type": "string",
                    "description": "Service id from opendevtui_list_services."
                },
                "tail": {
                    "type": "integer",
                    "description": "Optional maximum number of log entries to return.",
                    "minimum": 0
                }
            }))
        },
        {
            "name": "opendevtui_clear_logs",
            "description": "Clear logs for one service in the already-running OpenDevTUI instance.",
            "inputSchema": object_schema(json!({
                "service_id": {
                    "type": "string",
                    "description": "Service id from opendevtui_list_services."
                }
            }))
        }
    ])
}

fn object_schema(properties: Value) -> Value {
    let required = properties
        .as_object()
        .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn parse_http_json_response(response: &[u8]) -> Result<Value> {
    let Some(header_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        bail!("OpenDevTUI returned a malformed HTTP response");
    };
    let headers = std::str::from_utf8(&response[..header_end])
        .context("OpenDevTUI response headers are not UTF-8")?;
    let status_line = headers.lines().next().unwrap_or_default();
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse::<u16>().ok())
        .context("OpenDevTUI response missing HTTP status")?;
    let body = &response[header_end + 4..];
    let value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(body).context("OpenDevTUI response body is not JSON")?
    };
    if !(200..300).contains(&status_code) {
        bail!("OpenDevTUI API returned HTTP {status_code}: {value}");
    }
    Ok(value)
}

fn required_string_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("missing required string argument `{name}`"))
}

struct MessageReader<R> {
    input: R,
    buffer: Vec<u8>,
    framing: Option<Framing>,
}

#[derive(Clone, Copy, Debug)]
enum Framing {
    ContentLength,
    JsonLine,
}

impl<R: Read> MessageReader<R> {
    fn new(input: R) -> Self {
        Self {
            input,
            buffer: Vec::new(),
            framing: None,
        }
    }

    fn framing(&self) -> Framing {
        self.framing.unwrap_or(Framing::ContentLength)
    }

    fn read_message(&mut self) -> Result<Option<Value>> {
        loop {
            if let Some(message) = self.try_parse_message()? {
                return Ok(Some(message));
            }

            let mut chunk = [0_u8; 4096];
            let read = self
                .input
                .read(&mut chunk)
                .context("failed to read stdin")?;
            if read == 0 {
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                bail!("stdin closed with a partial MCP message");
            }
            trace(&format!("read {read} bytes"));
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    fn try_parse_message(&mut self) -> Result<Option<Value>> {
        if let Some(message) = self.try_parse_json_line()? {
            self.framing = Some(Framing::JsonLine);
            return Ok(Some(message));
        }

        let Some((header_end, separator_len)) = find_header_end(&self.buffer) else {
            return Ok(None);
        };
        self.framing = Some(Framing::ContentLength);
        let header = std::str::from_utf8(&self.buffer[..header_end])
            .context("MCP header is not valid UTF-8")?;
        let content_length = header
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .context("MCP message missing Content-Length header")?;
        let body_start = header_end + separator_len;
        let body_end = body_start + content_length;
        if self.buffer.len() < body_end {
            return Ok(None);
        }
        let body = self.buffer[body_start..body_end].to_vec();
        self.buffer.drain(..body_end);
        serde_json::from_slice(&body)
            .context("failed to parse MCP JSON message")
            .map(Some)
    }

    fn try_parse_json_line(&mut self) -> Result<Option<Value>> {
        let Some(line_end) = self.buffer.iter().position(|byte| *byte == b'\n') else {
            return Ok(None);
        };
        let line = &self.buffer[..line_end];
        if !line.trim_ascii_start().starts_with(b"{") {
            return Ok(None);
        }
        let body = line.strip_suffix(b"\r").unwrap_or(line).to_vec();
        self.buffer.drain(..=line_end);
        serde_json::from_slice(&body)
            .context("failed to parse MCP JSON line message")
            .map(Some)
    }
}

fn find_header_end(buffer: &[u8]) -> Option<(usize, usize)> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| (position, 4))
        .or_else(|| {
            buffer
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|position| (position, 2))
        })
}

fn write_message(writer: &mut impl Write, message: &Value, framing: Framing) -> Result<()> {
    let body = serde_json::to_vec(message).context("failed to serialize MCP response")?;
    trace(&format!(
        "sending {}",
        message
            .get("id")
            .map(Value::to_string)
            .unwrap_or_else(|| "<notification>".into())
    ));
    match framing {
        Framing::ContentLength => {
            write!(writer, "Content-Length: {}\r\n\r\n", body.len())
                .context("failed to write header")?;
            writer.write_all(&body).context("failed to write body")?;
        }
        Framing::JsonLine => {
            writer.write_all(&body).context("failed to write body")?;
            writer.write_all(b"\n").context("failed to write newline")?;
        }
    }
    writer.flush().context("failed to flush response")
}

fn trace(message: &str) {
    let Ok(path) = std::env::var("OPENDEVTUI_MCP_TRACE") else {
        return;
    };
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let _ = writeln!(file, "{message}");
}
