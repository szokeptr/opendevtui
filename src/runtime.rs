#[cfg(not(unix))]
compile_error!("opendevtui currently supports Unix terminals only");

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::process::Stdio;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::time::timeout;

use crate::config::ServiceConfig;

const DEFAULT_STOP_TIMEOUT_MS: u64 = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogKind {
    Stdout,
    Stderr,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogEntry {
    pub kind: LogKind,
    pub line: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ResourceUsage {
    pub cpu_percent: f32,
    pub memory_kib: u64,
}

#[derive(Debug, Clone)]
pub struct ServiceRuntime {
    pub pid: Option<u32>,
    pub status: ServiceStatus,
    pub logs: Vec<LogEntry>,
    pub exit_code: Option<i32>,
    pub resource_usage: Option<ResourceUsage>,
}

#[derive(Debug, Clone)]
pub enum RuntimeEvent {
    Started {
        service_id: String,
        pid: u32,
    },
    Log {
        service_id: String,
        stream: LogStream,
        line: String,
    },
    Exited {
        service_id: String,
        exit_code: Option<i32>,
    },
    RuntimeError {
        service_id: String,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
}

#[derive(Clone)]
pub struct RuntimeController {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    workspace_root: PathBuf,
    event_tx: mpsc::UnboundedSender<RuntimeEvent>,
    handles: Mutex<HashMap<String, ManagedService>>,
    stop_timeout: Duration,
}

#[derive(Clone)]
struct ManagedService {
    child: Arc<Mutex<Child>>,
    pgid: i32,
    exited: Arc<AtomicBool>,
    exit_notify: Arc<Notify>,
}

impl Default for ServiceRuntime {
    fn default() -> Self {
        Self {
            pid: None,
            status: ServiceStatus::Stopped,
            logs: Vec::new(),
            exit_code: None,
            resource_usage: None,
        }
    }
}

impl RuntimeController {
    pub fn new(workspace_root: PathBuf, event_tx: mpsc::UnboundedSender<RuntimeEvent>) -> Self {
        Self {
            inner: Arc::new(RuntimeInner {
                workspace_root,
                event_tx,
                handles: Mutex::new(HashMap::new()),
                stop_timeout: Duration::from_millis(DEFAULT_STOP_TIMEOUT_MS),
            }),
        }
    }

    pub fn with_stop_timeout(mut self, stop_timeout: Duration) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("stop timeout can only be set before cloning")
            .stop_timeout = stop_timeout;
        self
    }

    pub async fn start(&self, service: ServiceConfig) -> Result<()> {
        let mut handles = self.inner.handles.lock().await;
        if handles.contains_key(&service.id) {
            bail!("service `{}` is already running", service.id);
        }

        let cwd = service.resolved_cwd(&self.inner.workspace_root);
        let mut command = Command::new(&service.command);
        command
            .args(&service.args)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        for (key, value) in &service.env {
            command.env(key, value);
        }

        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start `{}`", service.display_name()))?;

        let pid = child
            .id()
            .with_context(|| format!("process `{}` did not report a pid", service.id))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let managed = ManagedService {
            child: Arc::new(Mutex::new(child)),
            pgid: pid as i32,
            exited: Arc::new(AtomicBool::new(false)),
            exit_notify: Arc::new(Notify::new()),
        };
        handles.insert(service.id.clone(), managed.clone());
        drop(handles);

        let _ = self.inner.event_tx.send(RuntimeEvent::Started {
            service_id: service.id.clone(),
            pid,
        });

        if let Some(stdout) = stdout {
            spawn_log_task(
                service.id.clone(),
                LogStream::Stdout,
                stdout,
                self.inner.event_tx.clone(),
            );
        }
        if let Some(stderr) = stderr {
            spawn_log_task(
                service.id.clone(),
                LogStream::Stderr,
                stderr,
                self.inner.event_tx.clone(),
            );
        }
        spawn_wait_task(service.id, managed, self.inner.clone());

        Ok(())
    }

    pub async fn stop(&self, service_id: &str) -> Result<bool> {
        let managed = {
            let handles = self.inner.handles.lock().await;
            handles.get(service_id).cloned()
        };
        let Some(managed) = managed else {
            return Ok(false);
        };

        let group = Pid::from_raw(managed.pgid);
        let _ = killpg(group, Signal::SIGINT);
        wait_for_exit(&managed, self.inner.stop_timeout).await?;
        if !managed.exited.load(Ordering::SeqCst) {
            let _ = killpg(group, Signal::SIGKILL);
            wait_for_exit(&managed, self.inner.stop_timeout).await?;
        }
        Ok(true)
    }

    pub async fn restart(&self, service: ServiceConfig) -> Result<()> {
        let _ = self.stop(&service.id).await?;
        self.start(service).await
    }

    pub async fn is_running(&self, service_id: &str) -> bool {
        self.inner.handles.lock().await.contains_key(service_id)
    }

    pub fn sample_resource_usage(&self, pids: &[u32]) -> Result<HashMap<u32, ResourceUsage>> {
        sample_resource_usage(pids)
    }

    pub fn sample_docker_resource_usage(
        &self,
        services: &[ServiceConfig],
    ) -> Result<HashMap<String, ResourceUsage>> {
        sample_docker_resource_usage(&self.inner.workspace_root, services)
    }
}

fn sample_resource_usage(pids: &[u32]) -> Result<HashMap<u32, ResourceUsage>> {
    if pids.is_empty() {
        return Ok(HashMap::new());
    }

    let pid_list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let output = StdCommand::new("ps")
        .args(["-o", "pid=", "-o", "%cpu=", "-o", "rss=", "-p", &pid_list])
        .output()
        .context("failed to run `ps` for resource usage")?;
    if !output.status.success() {
        bail!(
            "`ps` exited with {} while collecting resource usage",
            output.status
        );
    }

    parse_ps_resource_usage(&String::from_utf8_lossy(&output.stdout))
}

fn parse_ps_resource_usage(output: &str) -> Result<HashMap<u32, ResourceUsage>> {
    let mut usages = HashMap::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let mut parts = line.split_whitespace();
        let Some(pid) = parts.next() else {
            continue;
        };
        let Some(cpu_percent) = parts.next() else {
            continue;
        };
        let Some(memory_kib) = parts.next() else {
            continue;
        };

        let pid: u32 = pid
            .parse()
            .with_context(|| format!("invalid pid in `ps` output: {line}"))?;
        let cpu_percent: f32 = cpu_percent
            .parse()
            .with_context(|| format!("invalid cpu usage in `ps` output: {line}"))?;
        let memory_kib: u64 = memory_kib
            .parse()
            .with_context(|| format!("invalid rss usage in `ps` output: {line}"))?;

        usages.insert(
            pid,
            ResourceUsage {
                cpu_percent,
                memory_kib,
            },
        );
    }

    Ok(usages)
}

pub fn is_docker_service(service: &ServiceConfig) -> bool {
    docker_compose_ps_args(service).is_some()
}

fn sample_docker_resource_usage(
    workspace_root: &Path,
    services: &[ServiceConfig],
) -> Result<HashMap<String, ResourceUsage>> {
    let mut usages = HashMap::new();

    for service in services {
        let Some((compose_command, compose_ps_args)) = docker_compose_ps_args(service) else {
            continue;
        };
        let cwd = service.resolved_cwd(workspace_root);
        let container_output = docker_command(&compose_command, &compose_ps_args, service, &cwd)
            .with_context(|| format!("failed to list Docker containers for `{}`", service.id))?;
        if !container_output.status.success() {
            continue;
        }

        let container_ids = String::from_utf8_lossy(&container_output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        if container_ids.is_empty() {
            continue;
        }

        let mut stats_args = vec![
            "stats".to_string(),
            "--no-stream".to_string(),
            "--format".to_string(),
            "{{.CPUPerc}}\t{{.MemUsage}}".to_string(),
        ];
        stats_args.extend(container_ids);
        let stats_output = docker_command("docker", &stats_args, service, &cwd)
            .with_context(|| format!("failed to collect Docker stats for `{}`", service.id))?;
        if !stats_output.status.success() {
            continue;
        }

        usages.insert(
            service.id.clone(),
            parse_docker_stats_resource_usage(&String::from_utf8_lossy(&stats_output.stdout))?,
        );
    }

    Ok(usages)
}

fn docker_command(
    command: &str,
    args: &[String],
    service: &ServiceConfig,
    cwd: &Path,
) -> Result<std::process::Output> {
    let mut command = StdCommand::new(command);
    command.args(args).current_dir(cwd);
    for (key, value) in &service.env {
        command.env(key, value);
    }
    command.output().context("failed to run Docker command")
}

fn docker_compose_ps_args(service: &ServiceConfig) -> Option<(String, Vec<String>)> {
    let command = Path::new(&service.command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(service.command.as_str());

    match command {
        "docker" if service.args.first().map(String::as_str) == Some("compose") => {
            compose_ps_args(&service.args).map(|args| ("docker".into(), args))
        }
        "docker-compose" => {
            compose_ps_args(&service.args).map(|args| ("docker-compose".into(), args))
        }
        _ => None,
    }
}

fn compose_ps_args(args: &[String]) -> Option<Vec<String>> {
    let up_index = args.iter().position(|arg| arg == "up")?;
    let mut ps_args = args[..up_index].to_vec();
    ps_args.push("ps".into());
    ps_args.push("-q".into());
    Some(ps_args)
}

fn parse_docker_stats_resource_usage(output: &str) -> Result<ResourceUsage> {
    let mut usage = ResourceUsage {
        cpu_percent: 0.0,
        memory_kib: 0,
    };

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let mut parts = line.split('\t');
        let cpu_percent = parts
            .next()
            .context("missing cpu usage in Docker stats output")?;
        let memory_usage = parts
            .next()
            .context("missing memory usage in Docker stats output")?;

        usage.cpu_percent += parse_percent(cpu_percent)
            .with_context(|| format!("invalid cpu usage in Docker stats output: {line}"))?;
        usage.memory_kib += parse_docker_memory_kib(memory_usage)
            .with_context(|| format!("invalid memory usage in Docker stats output: {line}"))?;
    }

    Ok(usage)
}

fn parse_percent(value: &str) -> Result<f32> {
    value
        .trim()
        .trim_end_matches('%')
        .parse()
        .with_context(|| format!("invalid percent value: {value}"))
}

fn parse_docker_memory_kib(value: &str) -> Result<u64> {
    let current = value
        .split('/')
        .next()
        .map(str::trim)
        .context("missing current memory value")?;
    parse_memory_quantity_kib(current)
}

fn parse_memory_quantity_kib(value: &str) -> Result<u64> {
    let split_at = value
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit() && *ch != '.')
        .map(|(index, _)| index)
        .unwrap_or(value.len());
    let (amount, unit) = value.split_at(split_at);
    let amount: f64 = amount
        .parse()
        .with_context(|| format!("invalid memory quantity: {value}"))?;
    let kib = match unit.trim().to_ascii_lowercase().as_str() {
        "b" => amount / 1024.0,
        "kib" | "kb" | "k" => amount,
        "mib" | "mb" | "m" => amount * 1024.0,
        "gib" | "gb" | "g" => amount * 1024.0 * 1024.0,
        "tib" | "tb" | "t" => amount * 1024.0 * 1024.0 * 1024.0,
        "" => amount / 1024.0,
        _ => bail!("unsupported memory unit in Docker stats output: {value}"),
    };
    Ok(kib.round() as u64)
}

async fn wait_for_exit(managed: &ManagedService, duration: Duration) -> Result<()> {
    if managed.exited.load(Ordering::SeqCst) {
        return Ok(());
    }
    let notified = managed.exit_notify.notified();
    let _ = timeout(duration, async {
        if !managed.exited.load(Ordering::SeqCst) {
            notified.await;
        }
    })
    .await;
    Ok(())
}

fn spawn_log_task<R>(
    service_id: String,
    stream: LogStream,
    reader: R,
    event_tx: mpsc::UnboundedSender<RuntimeEvent>,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    let line = sanitize_log_line(&line);
                    let _ = event_tx.send(RuntimeEvent::Log {
                        service_id: service_id.clone(),
                        stream,
                        line,
                    });
                }
                Ok(None) => break,
                Err(err) => {
                    let _ = event_tx.send(RuntimeEvent::RuntimeError {
                        service_id: service_id.clone(),
                        message: format!("log stream error: {err}"),
                    });
                    break;
                }
            }
        }
    });
}

pub fn sanitize_log_line(input: &str) -> String {
    let no_ansi = strip_ansi_sequences(input);
    let mut output = String::with_capacity(no_ansi.len());
    let mut last_was_space = false;

    for ch in no_ansi.chars() {
        match ch {
            '\t' => {
                if !last_was_space {
                    output.push(' ');
                    last_was_space = true;
                }
            }
            '\r' | '\u{0008}' => {}
            ch if ch.is_control() => {}
            ch => {
                output.push(ch);
                last_was_space = ch == ' ';
            }
        }
    }

    output.trim_end().to_string()
}

fn strip_ansi_sequences(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i >= bytes.len() {
                break;
            }
            match bytes[i] {
                b'[' => {
                    i += 1;
                    while i < bytes.len() {
                        let b = bytes[i];
                        i += 1;
                        if (0x40..=0x7e).contains(&b) {
                            break;
                        }
                    }
                }
                b']' => {
                    i += 1;
                    while i < bytes.len() {
                        match bytes[i] {
                            0x07 => {
                                i += 1;
                                break;
                            }
                            0x1b if i + 1 < bytes.len() && bytes[i + 1] == b'\\' => {
                                i += 2;
                                break;
                            }
                            _ => i += 1,
                        }
                    }
                }
                _ => {
                    i += 1;
                }
            }
            continue;
        }

        if let Some(ch) = input[i..].chars().next() {
            output.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }

    output
}

fn spawn_wait_task(service_id: String, managed: ManagedService, inner: Arc<RuntimeInner>) {
    tokio::spawn(async move {
        let exit = managed.child.lock().await.wait().await;
        managed.exited.store(true, Ordering::SeqCst);
        managed.exit_notify.notify_waiters();
        inner.handles.lock().await.remove(&service_id);

        match exit {
            Ok(status) => {
                let _ = inner.event_tx.send(RuntimeEvent::Exited {
                    service_id,
                    exit_code: status.code(),
                });
            }
            Err(err) => {
                let _ = inner.event_tx.send(RuntimeEvent::RuntimeError {
                    service_id,
                    message: err.to_string(),
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;
    use std::fs;

    use tempfile::tempdir;

    fn test_service(root: &std::path::Path, script: &str) -> ServiceConfig {
        let cwd = root.join("service");
        fs::create_dir_all(&cwd).unwrap();
        ServiceConfig {
            id: "svc".into(),
            name: "svc".into(),
            cwd: "service".into(),
            command: "bash".into(),
            args: vec!["-lc".into(), script.into()],
            env: BTreeMap::new(),
            autostart: false,
        }
    }

    async fn collect_until_exit(
        rx: &mut mpsc::UnboundedReceiver<RuntimeEvent>,
    ) -> Vec<RuntimeEvent> {
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            let done = matches!(event, RuntimeEvent::Exited { .. });
            events.push(event);
            if done {
                break;
            }
        }
        events
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn captures_logs_and_exit() {
        let dir = tempdir().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let controller = RuntimeController::new(dir.path().to_path_buf(), tx);
        let service = test_service(dir.path(), "echo out; echo err 1>&2; sleep 0.1");

        controller.start(service).await.unwrap();
        let events = collect_until_exit(&mut rx).await;

        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Started { service_id, .. } if service_id == "svc"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Log {
                service_id,
                stream: LogStream::Stdout,
                line
            } if service_id == "svc" && line == "out"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::Log {
                service_id,
                stream: LogStream::Stderr,
                line
            } if service_id == "svc" && line == "err"
        )));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stop_falls_back_to_force_kill() {
        let dir = tempdir().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let controller = RuntimeController::new(dir.path().to_path_buf(), tx)
            .with_stop_timeout(Duration::from_millis(150));
        let service = test_service(dir.path(), "trap '' INT; while true; do sleep 1; done");

        controller.start(service).await.unwrap();
        rx.recv().await.unwrap();
        controller.stop("svc").await.unwrap();
        let events = collect_until_exit(&mut rx).await;

        assert!(events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Exited { .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn restart_relaunches_service() {
        let dir = tempdir().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let controller = RuntimeController::new(dir.path().to_path_buf(), tx);
        let service = test_service(dir.path(), "sleep 10");

        controller.start(service.clone()).await.unwrap();
        rx.recv().await.unwrap();
        controller.restart(service).await.unwrap();

        let first = rx.recv().await.unwrap();
        let second = rx.recv().await.unwrap();
        assert!(
            matches!(first, RuntimeEvent::Exited { .. })
                || matches!(second, RuntimeEvent::Exited { .. })
        );
        assert!(
            matches!(first, RuntimeEvent::Started { .. })
                || matches!(second, RuntimeEvent::Started { .. })
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn env_and_cwd_are_applied() {
        let dir = tempdir().unwrap();
        let subdir = dir.path().join("service");
        fs::create_dir_all(&subdir).unwrap();
        let expected_pwd = subdir.canonicalize().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let controller = RuntimeController::new(dir.path().to_path_buf(), tx);
        let mut service = test_service(dir.path(), "echo \"$TEST_VALUE\"; pwd");
        service.env.insert("TEST_VALUE".into(), "works".into());

        controller.start(service).await.unwrap();
        let events = collect_until_exit(&mut rx).await;
        let mut saw_env = false;
        let mut saw_pwd = false;
        for event in events {
            if let RuntimeEvent::Log { line, .. } = event {
                saw_env |= line == "works";
                saw_pwd |= PathBuf::from(&line).canonicalize().ok().as_ref() == Some(&expected_pwd);
            }
        }

        assert!(saw_env);
        assert!(saw_pwd);
    }

    #[test]
    fn sanitize_strips_ansi_tabs_and_osc_sequences() {
        let line =
            "\u{1b}[32mgreen\u{1b}[0m\tvalue\u{1b}]8;;https://example.com\u{7}link\u{1b}]8;;\u{7}";
        assert_eq!(sanitize_log_line(line), "green valuelink");
    }

    #[test]
    fn parse_ps_resource_usage_reads_pid_cpu_and_memory() {
        let usage = parse_ps_resource_usage("123 12.5 4096\n456 0.0 128\n").unwrap();

        assert_eq!(
            usage.get(&123),
            Some(&ResourceUsage {
                cpu_percent: 12.5,
                memory_kib: 4096,
            })
        );
        assert_eq!(
            usage.get(&456),
            Some(&ResourceUsage {
                cpu_percent: 0.0,
                memory_kib: 128,
            })
        );
    }

    #[test]
    fn docker_compose_ps_args_replaces_up_with_ps() {
        let mut service = test_service(std::path::Path::new("/tmp"), "sleep 1");
        service.command = "docker".into();
        service.args = vec![
            "compose".into(),
            "-f".into(),
            "compose.dev.yml".into(),
            "up".into(),
        ];

        let (command, args) = docker_compose_ps_args(&service).unwrap();

        assert_eq!(command, "docker");
        assert_eq!(args, vec!["compose", "-f", "compose.dev.yml", "ps", "-q"]);
        assert!(is_docker_service(&service));
    }

    #[test]
    fn docker_compose_ps_args_supports_legacy_binary() {
        let mut service = test_service(std::path::Path::new("/tmp"), "sleep 1");
        service.command = "docker-compose".into();
        service.args = vec!["up".into()];

        let (command, args) = docker_compose_ps_args(&service).unwrap();

        assert_eq!(command, "docker-compose");
        assert_eq!(args, vec!["ps", "-q"]);
    }

    #[test]
    fn parse_docker_stats_resource_usage_sums_containers() {
        let usage =
            parse_docker_stats_resource_usage("1.25%\t10.5MiB / 1GiB\n0.75%\t512KiB / 1GiB\n")
                .unwrap();

        assert_eq!(
            usage,
            ResourceUsage {
                cpu_percent: 2.0,
                memory_kib: 11_264,
            }
        );
    }
}
