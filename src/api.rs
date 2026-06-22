use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use serde::{Deserialize, Serialize};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::config::ServiceConfig;
use crate::runtime::{LogEntry, ResourceUsage, ServiceStatus};

pub const SOCKET_FILE: &str = "opendevtui.sock";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ServiceSnapshot {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub command: String,
    pub args: Vec<String>,
    pub autostart: bool,
    pub status: ServiceStatus,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub resource_usage: Option<ResourceUsage>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LogSnapshot {
    pub index: usize,
    pub kind: crate::runtime::LogKind,
    pub line: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LogsResponse {
    pub service: ServiceSnapshot,
    pub logs: Vec<LogSnapshot>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ServiceActionResponse {
    pub message: String,
    pub service: ServiceSnapshot,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ApiError {
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn not_found(service_id: &str) -> Self {
        Self {
            code: "not_found",
            message: format!("unknown service `{service_id}`"),
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            code: "operation_failed",
            message: message.into(),
        }
    }

    fn status(&self) -> StatusCode {
        match self.code {
            "not_found" => StatusCode::NOT_FOUND,
            _ => StatusCode::BAD_REQUEST,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status(), Json(self)).into_response()
    }
}

pub enum ApiCommand {
    List {
        respond_to: oneshot::Sender<std::result::Result<Vec<ServiceSnapshot>, ApiError>>,
    },
    Get {
        service_id: String,
        respond_to: oneshot::Sender<std::result::Result<ServiceSnapshot, ApiError>>,
    },
    Start {
        service_id: String,
        respond_to: oneshot::Sender<std::result::Result<ServiceActionResponse, ApiError>>,
    },
    Stop {
        service_id: String,
        respond_to: oneshot::Sender<std::result::Result<ServiceActionResponse, ApiError>>,
    },
    Restart {
        service_id: String,
        respond_to: oneshot::Sender<std::result::Result<ServiceActionResponse, ApiError>>,
    },
    Logs {
        service_id: String,
        tail: Option<usize>,
        respond_to: oneshot::Sender<std::result::Result<LogsResponse, ApiError>>,
    },
    ClearLogs {
        service_id: String,
        respond_to: oneshot::Sender<std::result::Result<ServiceActionResponse, ApiError>>,
    },
}

#[derive(Clone)]
struct ApiState {
    tx: mpsc::UnboundedSender<ApiCommand>,
}

pub struct ApiServer {
    pub socket_path: PathBuf,
    task: JoinHandle<()>,
}

impl fmt::Debug for ApiServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiServer")
            .field("socket_path", &self.socket_path)
            .finish_non_exhaustive()
    }
}

impl Drop for ApiServer {
    fn drop(&mut self) {
        self.task.abort();
        let _ = fs::remove_file(&self.socket_path);
    }
}

#[derive(Debug, Deserialize)]
struct LogsQuery {
    tail: Option<usize>,
}

pub async fn start_server(
    workspace_root: &Path,
    tx: mpsc::UnboundedSender<ApiCommand>,
) -> Result<ApiServer> {
    let state = ApiState { tx };
    let app = router(state);
    let socket_path = project_socket_path(workspace_root);
    prepare_socket_path(&socket_path)?;
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind control API to {}", socket_path.display()))?;
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _addr)) = listener.accept().await else {
                break;
            };
            let app = app.clone();
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let service = TowerToHyperService::new(app);
                if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                    eprintln!("control API Unix socket connection exited: {err}");
                }
            });
        }
    });

    Ok(ApiServer { socket_path, task })
}

pub fn project_socket_path(workspace_root: &Path) -> PathBuf {
    let mut path = PathBuf::from("/tmp");
    let segments = sanitized_workspace_segments(workspace_root);
    if segments.len() >= 2 {
        path.push(format!("{}-{}", segments[0], segments[1]));
        for segment in &segments[2..] {
            path.push(segment);
        }
    } else {
        for segment in segments {
            path.push(segment);
        }
    }
    path.push(SOCKET_FILE);
    if path.as_os_str().len() >= 100 {
        let mut short_path = PathBuf::from("/tmp");
        short_path.push(format!("opendevtui-{:016x}", path_hash(workspace_root)));
        short_path.push(SOCKET_FILE);
        return short_path;
    }
    path
}

fn sanitized_workspace_segments(workspace_root: &Path) -> Vec<String> {
    workspace_root
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => {
                let segment = slugify_path_segment(&value.to_string_lossy());
                (!segment.is_empty()).then_some(segment)
            }
            _ => None,
        })
        .collect()
}

fn slugify_path_segment(segment: &str) -> String {
    let mut out = String::new();
    let mut last_was_dash = false;
    for ch in segment.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn path_hash(path: &Path) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in path.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn prepare_socket_path(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .context("socket path must have a parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => bail!("failed to remove stale socket {}: {err}", path.display()),
    }
}

fn router(state: ApiState) -> Router {
    Router::new()
        .route("/services", get(list_services))
        .route("/services/:id", get(get_service))
        .route("/services/:id/start", post(start_service))
        .route("/services/:id/stop", post(stop_service))
        .route("/services/:id/restart", post(restart_service))
        .route("/services/:id/logs", get(get_logs))
        .route("/services/:id/logs/clear", post(clear_logs))
        .with_state(state)
}

async fn list_services(
    State(state): State<ApiState>,
) -> std::result::Result<Json<Vec<ServiceSnapshot>>, ApiError> {
    call(&state.tx, |respond_to| ApiCommand::List { respond_to })
        .await
        .map(Json)
}

async fn get_service(
    State(state): State<ApiState>,
    AxumPath(service_id): AxumPath<String>,
) -> std::result::Result<Json<ServiceSnapshot>, ApiError> {
    call(&state.tx, |respond_to| ApiCommand::Get {
        service_id,
        respond_to,
    })
    .await
    .map(Json)
}

async fn start_service(
    State(state): State<ApiState>,
    AxumPath(service_id): AxumPath<String>,
) -> std::result::Result<Json<ServiceActionResponse>, ApiError> {
    call(&state.tx, |respond_to| ApiCommand::Start {
        service_id,
        respond_to,
    })
    .await
    .map(Json)
}

async fn stop_service(
    State(state): State<ApiState>,
    AxumPath(service_id): AxumPath<String>,
) -> std::result::Result<Json<ServiceActionResponse>, ApiError> {
    call(&state.tx, |respond_to| ApiCommand::Stop {
        service_id,
        respond_to,
    })
    .await
    .map(Json)
}

async fn restart_service(
    State(state): State<ApiState>,
    AxumPath(service_id): AxumPath<String>,
) -> std::result::Result<Json<ServiceActionResponse>, ApiError> {
    call(&state.tx, |respond_to| ApiCommand::Restart {
        service_id,
        respond_to,
    })
    .await
    .map(Json)
}

async fn get_logs(
    State(state): State<ApiState>,
    AxumPath(service_id): AxumPath<String>,
    Query(query): Query<LogsQuery>,
) -> std::result::Result<Json<LogsResponse>, ApiError> {
    call(&state.tx, |respond_to| ApiCommand::Logs {
        service_id,
        tail: query.tail,
        respond_to,
    })
    .await
    .map(Json)
}

async fn clear_logs(
    State(state): State<ApiState>,
    AxumPath(service_id): AxumPath<String>,
) -> std::result::Result<Json<ServiceActionResponse>, ApiError> {
    call(&state.tx, |respond_to| ApiCommand::ClearLogs {
        service_id,
        respond_to,
    })
    .await
    .map(Json)
}

async fn call<T>(
    tx: &mpsc::UnboundedSender<ApiCommand>,
    build: impl FnOnce(oneshot::Sender<std::result::Result<T, ApiError>>) -> ApiCommand,
) -> std::result::Result<T, ApiError> {
    let (respond_to, response) = oneshot::channel();
    tx.send(build(respond_to))
        .map_err(|_| ApiError::failed("control loop is not running"))?;
    response
        .await
        .map_err(|_| ApiError::failed("control loop dropped the response"))?
}

pub fn service_snapshot(
    config: &ServiceConfig,
    runtime: &crate::runtime::ServiceRuntime,
) -> ServiceSnapshot {
    ServiceSnapshot {
        id: config.id.clone(),
        name: config.name.clone(),
        cwd: config.cwd.clone(),
        command: config.command.clone(),
        args: config.args.clone(),
        autostart: config.autostart,
        status: runtime.status,
        pid: runtime.pid,
        exit_code: runtime.exit_code,
        resource_usage: runtime.resource_usage,
    }
}

pub fn log_snapshots(logs: &[LogEntry], tail: Option<usize>) -> Vec<LogSnapshot> {
    let start = tail
        .map(|tail| logs.len().saturating_sub(tail))
        .unwrap_or(0);
    logs.iter()
        .enumerate()
        .skip(start)
        .map(|(index, entry)| LogSnapshot {
            index,
            kind: entry.kind,
            line: entry.line.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn log_snapshots_preserve_indices_when_tailing() {
        let logs = vec![
            LogEntry {
                kind: crate::runtime::LogKind::Stdout,
                line: "one".into(),
            },
            LogEntry {
                kind: crate::runtime::LogKind::Stderr,
                line: "two".into(),
            },
            LogEntry {
                kind: crate::runtime::LogKind::System,
                line: "three".into(),
            },
        ];

        let snapshots = log_snapshots(&logs, Some(2));

        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].index, 1);
        assert_eq!(snapshots[1].index, 2);
    }

    #[test]
    fn project_socket_path_slugifies_workspace_path() {
        assert_eq!(
            project_socket_path(Path::new("/Users/szokeptr/code/bla")),
            PathBuf::from("/tmp")
                .join("users-szokeptr")
                .join("code")
                .join("bla")
                .join(SOCKET_FILE)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unix_socket_server_routes_list_services() {
        let dir = tempdir().unwrap();
        let socket_path = project_socket_path(dir.path());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let server = start_server(dir.path(), tx).await.unwrap();
        assert_eq!(server.socket_path, socket_path);

        let responder = tokio::spawn(async move {
            let Some(ApiCommand::List { respond_to }) = rx.recv().await else {
                panic!("expected list command");
            };
            respond_to
                .send(Ok(vec![ServiceSnapshot {
                    id: "api".into(),
                    name: "API".into(),
                    cwd: ".".into(),
                    command: "echo".into(),
                    args: vec!["ok".into()],
                    autostart: false,
                    status: ServiceStatus::Stopped,
                    pid: None,
                    exit_code: None,
                    resource_usage: None,
                }]))
                .unwrap();
        });

        let authorized = raw_unix_get(&socket_path, "/services").await;
        responder.await.unwrap();

        assert!(authorized.starts_with("HTTP/1.1 200 OK"));
        assert!(authorized.contains(r#""id":"api""#));
        drop(server);
        assert!(!socket_path.exists());
    }

    async fn raw_unix_get(socket_path: &Path, path: &str) -> String {
        let mut stream = tokio::net::UnixStream::connect(socket_path).await.unwrap();
        raw_get(&mut stream, "localhost", path).await
    }

    async fn raw_get<S>(stream: &mut S, host: &str, path: &str) -> String
    where
        S: AsyncReadExt + AsyncWriteExt + Unpin,
    {
        let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        String::from_utf8(response).unwrap()
    }
}
