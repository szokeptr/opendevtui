use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, size, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::api::{self, ApiCommand, ApiError, ApiServer, LogsResponse, ServiceActionResponse};
use crate::config::{AppConfig, ConfigStore, ServiceConfig, CONFIG_VERSION};
use crate::editor::{
    EditorState, FormEditorState, FormField, FormMode, RawConfigEditorState, ServicePreset,
};
use crate::runtime::{
    is_docker_service, sanitize_log_line, LogEntry, LogKind, LogStream, RuntimeController,
    RuntimeEvent, ServiceRuntime, ServiceStatus,
};
use crate::ui;

const UI_POLL_MS: u64 = 50;
const MAX_LOG_LINES: usize = 500;
const MOUSE_SCROLL_LINES: i16 = 1;
const PAGE_SCROLL_LINES: i16 = 8;
const RESOURCE_REFRESH_MS: u64 = 1_000;
const STATUS_MESSAGE_TTL: Duration = Duration::from_millis(2_500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPane {
    Services,
    Details,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum RightPaneMode {
    Logs,
    PresetPicker(PresetPickerState),
    Editor(EditorState),
    ConfirmDelete,
}

#[derive(Debug, Clone)]
pub struct PresetPickerState {
    pub selected: usize,
}

#[derive(Debug, Clone)]
pub struct ServiceEntry {
    pub config: ServiceConfig,
    pub runtime: ServiceRuntime,
    pub log_scroll: u16,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub workspace_root: PathBuf,
    pub config_path: PathBuf,
    pub services: Vec<ServiceEntry>,
    pub selected_service: usize,
    pub focus: FocusPane,
    pub right_pane: RightPaneMode,
    pub wrap_logs: bool,
    pub status_message: Option<String>,
    pub status_message_expires_at: Option<Instant>,
}

pub struct App {
    pub state: AppState,
    store: ConfigStore,
    runtime: RuntimeController,
    runtime_rx: mpsc::UnboundedReceiver<RuntimeEvent>,
    api_rx: Option<mpsc::UnboundedReceiver<ApiCommand>>,
    api_server: Option<ApiServer>,
    last_resource_refresh: Instant,
}

impl AppState {
    pub fn selected_service(&self) -> Option<&ServiceEntry> {
        self.services.get(self.selected_service)
    }

    pub fn selected_service_mut(&mut self) -> Option<&mut ServiceEntry> {
        self.services.get_mut(self.selected_service)
    }

    pub fn config(&self) -> AppConfig {
        AppConfig {
            version: CONFIG_VERSION,
            services: self
                .services
                .iter()
                .map(|service| service.config.clone())
                .collect(),
        }
    }
}

impl App {
    pub async fn load(workspace_root: PathBuf) -> Result<Self> {
        let store = ConfigStore::new(workspace_root.clone());
        let config = store.load()?;
        let services = config
            .services
            .into_iter()
            .map(|config| ServiceEntry {
                config,
                runtime: ServiceRuntime::default(),
                log_scroll: 0,
            })
            .collect();
        let (runtime_tx, runtime_rx) = mpsc::unbounded_channel();
        let runtime = RuntimeController::new(workspace_root.clone(), runtime_tx);
        Ok(Self {
            state: AppState {
                workspace_root,
                config_path: store.config_path(),
                services,
                selected_service: 0,
                focus: FocusPane::Services,
                right_pane: RightPaneMode::Logs,
                wrap_logs: false,
                status_message: None,
                status_message_expires_at: None,
            },
            store,
            runtime,
            runtime_rx,
            api_rx: None,
            api_server: None,
            last_resource_refresh: Instant::now(),
        })
    }

    pub async fn enable_api(&mut self) -> Result<PathBuf> {
        let (api_tx, api_rx) = mpsc::unbounded_channel();
        let server = api::start_server(&self.state.workspace_root, api_tx).await?;
        let socket_path = server.socket_path.clone();
        self.set_status_message(format!(
            "control API socket listening at {}",
            socket_path.display()
        ));
        self.api_rx = Some(api_rx);
        self.api_server = Some(server);
        Ok(socket_path)
    }

    pub async fn run(&mut self) -> Result<()> {
        let mut terminal = setup_terminal()?;
        self.autostart().await;

        let loop_result = async {
            loop {
                self.drain_runtime_events();
                self.drain_api_requests().await;
                if self.last_resource_refresh.elapsed()
                    >= Duration::from_millis(RESOURCE_REFRESH_MS)
                {
                    self.refresh_resource_usage();
                    self.last_resource_refresh = Instant::now();
                }
                self.clear_expired_status_message();
                terminal.draw(|frame| {
                    let cursor = ui::render(frame, &self.state);
                    if let Some((x, y)) = cursor {
                        frame.set_cursor_position((x, y));
                    }
                })?;

                if event::poll(Duration::from_millis(UI_POLL_MS))? {
                    match event::read()? {
                        Event::Key(key) => {
                            if key.kind == KeyEventKind::Press && !self.handle_key(key).await? {
                                break;
                            }
                        }
                        Event::Mouse(mouse) => self.handle_mouse(mouse),
                        _ => {}
                    }
                }
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;

        let shutdown_result = self.shutdown().await;
        restore_terminal(&mut terminal)?;
        loop_result?;
        shutdown_result
    }

    pub async fn run_headless(&mut self) -> Result<()> {
        self.autostart().await;
        loop {
            self.drain_runtime_events();
            self.drain_api_requests().await;
            if self.last_resource_refresh.elapsed() >= Duration::from_millis(RESOURCE_REFRESH_MS) {
                self.refresh_resource_usage();
                self.last_resource_refresh = Instant::now();
            }

            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(UI_POLL_MS)) => {}
                _ = shutdown_signal() => break,
            }
        }
        self.shutdown().await
    }

    fn set_status_message(&mut self, message: impl Into<String>) {
        self.state.status_message = Some(message.into());
        self.state.status_message_expires_at = Some(Instant::now() + STATUS_MESSAGE_TTL);
    }

    fn clear_status_message(&mut self) {
        self.state.status_message = None;
        self.state.status_message_expires_at = None;
    }

    fn clear_expired_status_message(&mut self) {
        if self
            .state
            .status_message_expires_at
            .is_some_and(|expires_at| expires_at <= Instant::now())
        {
            self.clear_status_message();
        }
    }

    async fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(false);
        }

        match key.code {
            KeyCode::Tab => {
                self.state.focus = match self.state.focus {
                    FocusPane::Services => FocusPane::Details,
                    FocusPane::Details => FocusPane::Services,
                };
                return Ok(true);
            }
            KeyCode::Esc => {
                self.handle_escape();
                return Ok(true);
            }
            _ => {}
        }

        if matches!(self.state.right_pane, RightPaneMode::Logs) && key.code == KeyCode::Char('q') {
            return Ok(false);
        }

        match self.state.right_pane {
            RightPaneMode::Logs => self.handle_logs_mode(key).await?,
            RightPaneMode::PresetPicker(_) => self.handle_preset_picker(key)?,
            RightPaneMode::Editor(_) => self.handle_editor_mode(key)?,
            RightPaneMode::ConfirmDelete => self.handle_confirm_delete(key)?,
        }

        Ok(true)
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if !matches!(self.state.right_pane, RightPaneMode::Logs) {
            return;
        }
        if !mouse_targets_logs_pane(mouse) {
            return;
        }

        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.state.focus = FocusPane::Details;
                self.adjust_log_scroll(MOUSE_SCROLL_LINES);
            }
            MouseEventKind::ScrollDown => {
                self.state.focus = FocusPane::Details;
                self.adjust_log_scroll(-MOUSE_SCROLL_LINES);
            }
            _ => {}
        }
    }

    fn handle_escape(&mut self) {
        match &mut self.state.right_pane {
            RightPaneMode::Logs => {
                self.state.focus = FocusPane::Services;
            }
            RightPaneMode::PresetPicker(_) | RightPaneMode::ConfirmDelete => {
                self.state.right_pane = RightPaneMode::Logs;
                self.state.focus = FocusPane::Services;
            }
            RightPaneMode::Editor(EditorState::Form(editor)) => {
                if editor.is_editing {
                    editor.stop_editing();
                    editor.error = None;
                } else {
                    self.state.right_pane = RightPaneMode::Logs;
                    self.state.focus = FocusPane::Services;
                }
            }
            RightPaneMode::Editor(EditorState::Raw(_)) => {
                self.state.right_pane = RightPaneMode::Logs;
                self.state.focus = FocusPane::Services;
            }
        }
    }

    async fn handle_logs_mode(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('k') => {
                self.move_selection_up();
            }
            KeyCode::Char('j') => {
                self.move_selection_down();
            }
            KeyCode::Up if self.state.focus == FocusPane::Services => {
                self.move_selection_up();
            }
            KeyCode::Down if self.state.focus == FocusPane::Services => {
                self.move_selection_down();
            }
            KeyCode::Up if self.state.focus == FocusPane::Details => {
                self.adjust_log_scroll(1);
            }
            KeyCode::Down if self.state.focus == FocusPane::Details => {
                self.adjust_log_scroll(-1);
            }
            KeyCode::PageUp if self.state.focus == FocusPane::Details => {
                self.adjust_log_scroll(PAGE_SCROLL_LINES);
            }
            KeyCode::PageDown if self.state.focus == FocusPane::Details => {
                self.adjust_log_scroll(-PAGE_SCROLL_LINES);
            }
            KeyCode::Home if self.state.focus == FocusPane::Details => {
                self.scroll_logs_to_oldest();
            }
            KeyCode::End if self.state.focus == FocusPane::Details => {
                self.scroll_logs_to_latest();
            }
            KeyCode::Char('s') => self.start_selected_service().await?,
            KeyCode::Char('x') => self.stop_selected_service().await?,
            KeyCode::Char('r') => self.restart_selected_service().await?,
            KeyCode::Char('w') => self.toggle_log_wrap(),
            KeyCode::Char('C') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.clear_selected_logs()
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.clear_selected_logs()
            }
            KeyCode::Char('e') => self.open_selected_service_editor()?,
            KeyCode::Char('a') => {
                self.state.right_pane =
                    RightPaneMode::PresetPicker(PresetPickerState { selected: 0 });
                self.state.focus = FocusPane::Details;
            }
            KeyCode::Char('d') => {
                if self.state.selected_service().is_some() {
                    self.state.right_pane = RightPaneMode::ConfirmDelete;
                    self.state.focus = FocusPane::Details;
                }
            }
            KeyCode::Char('v') => self.open_raw_config_editor()?,
            _ => {}
        }
        Ok(())
    }

    fn handle_preset_picker(&mut self, key: KeyEvent) -> Result<()> {
        let RightPaneMode::PresetPicker(state) = &mut self.state.right_pane else {
            return Ok(());
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if state.selected == 0 {
                    state.selected = ServicePreset::ALL.len() - 1;
                } else {
                    state.selected -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.selected = (state.selected + 1) % ServicePreset::ALL.len();
            }
            KeyCode::Enter => {
                let preset = ServicePreset::ALL[state.selected];
                let service = preset.into_service();
                self.state.right_pane = RightPaneMode::Editor(EditorState::Form(
                    FormEditorState::new(FormMode::Create, preset, service),
                ));
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_editor_mode(&mut self, key: KeyEvent) -> Result<()> {
        match &self.state.right_pane {
            RightPaneMode::Editor(EditorState::Form(_)) => self.handle_form_editor(key),
            RightPaneMode::Editor(EditorState::Raw(_)) => self.handle_raw_editor(key),
            _ => Ok(()),
        }
    }

    fn handle_form_editor(&mut self, key: KeyEvent) -> Result<()> {
        let RightPaneMode::Editor(EditorState::Form(editor)) = &mut self.state.right_pane else {
            return Ok(());
        };
        if editor.is_editing {
            let selected_field = editor.selected_field;
            let allow_newline = selected_field.is_multiline();
            let Some(buffer) = editor.active_buffer_mut() else {
                editor.is_editing = false;
                return Ok(());
            };
            match key.code {
                KeyCode::Enter if !allow_newline => {
                    editor.stop_editing();
                }
                _ => {
                    buffer.handle_key(key, allow_newline);
                }
            }
            return Ok(());
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => editor.move_prev(),
            KeyCode::Down | KeyCode::Char('j') => editor.move_next(),
            KeyCode::Enter | KeyCode::Char('i') => {
                if editor.selected_field == FormField::Autostart {
                    editor.toggle_autostart();
                } else {
                    editor.start_editing();
                }
            }
            KeyCode::Char(' ') if editor.selected_field == FormField::Autostart => {
                editor.toggle_autostart();
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.save_form_editor()?;
            }
            _ => {}
        }

        Ok(())
    }

    fn handle_raw_editor(&mut self, key: KeyEvent) -> Result<()> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            self.save_raw_editor()?;
            return Ok(());
        }

        let RightPaneMode::Editor(EditorState::Raw(raw)) = &mut self.state.right_pane else {
            return Ok(());
        };
        raw.buffer.handle_key(key, true);
        Ok(())
    }

    fn handle_confirm_delete(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => self.delete_selected_service()?,
            KeyCode::Char('n') => {
                self.state.right_pane = RightPaneMode::Logs;
                self.state.focus = FocusPane::Services;
            }
            _ => {}
        }
        Ok(())
    }

    fn move_selection_up(&mut self) {
        if self.state.services.is_empty() {
            return;
        }
        if self.state.selected_service == 0 {
            self.state.selected_service = self.state.services.len() - 1;
        } else {
            self.state.selected_service -= 1;
        }
    }

    fn move_selection_down(&mut self) {
        if self.state.services.is_empty() {
            return;
        }
        self.state.selected_service = (self.state.selected_service + 1) % self.state.services.len();
    }

    fn adjust_log_scroll(&mut self, delta: i16) {
        if let Some(service) = self.state.selected_service_mut() {
            if delta < 0 {
                service.log_scroll = service.log_scroll.saturating_sub((-delta) as u16);
            } else {
                let max_scroll = service.runtime.logs.len().saturating_sub(1) as u16;
                service.log_scroll = service
                    .log_scroll
                    .saturating_add(delta as u16)
                    .min(max_scroll);
            }
        }
    }

    fn scroll_logs_to_oldest(&mut self) {
        if let Some(service) = self.state.selected_service_mut() {
            service.log_scroll = service.runtime.logs.len().saturating_sub(1) as u16;
        }
    }

    fn scroll_logs_to_latest(&mut self) {
        if let Some(service) = self.state.selected_service_mut() {
            service.log_scroll = 0;
        }
    }

    fn clear_selected_logs(&mut self) {
        let Some(index) = self.selected_service_index() else {
            return;
        };
        self.clear_service_logs(index);
    }

    fn clear_service_logs(&mut self, index: usize) {
        let service = &mut self.state.services[index];
        service.runtime.logs.clear();
        service.log_scroll = 0;
        let display_name = service.config.display_name().to_string();
        self.set_status_message(format!("cleared logs for `{display_name}`"));
    }

    fn toggle_log_wrap(&mut self) {
        self.state.wrap_logs = !self.state.wrap_logs;
        self.set_status_message(format!(
            "log wrap {}",
            if self.state.wrap_logs {
                "enabled"
            } else {
                "disabled"
            }
        ));
    }

    fn refresh_resource_usage(&mut self) {
        for service in &mut self.state.services {
            if !is_runtime_active(service.runtime.status) || service.runtime.pid.is_none() {
                service.runtime.resource_usage = None;
            }
        }

        let docker_services: Vec<ServiceConfig> = self
            .state
            .services
            .iter()
            .filter(|service| is_runtime_active(service.runtime.status))
            .filter(|service| is_docker_service(&service.config))
            .map(|service| service.config.clone())
            .collect();
        let docker_usages = if docker_services.is_empty() {
            HashMap::new()
        } else {
            self.runtime
                .sample_docker_resource_usage(&docker_services)
                .unwrap_or_default()
        };

        let pids: Vec<u32> = self
            .state
            .services
            .iter()
            .filter(|service| is_runtime_active(service.runtime.status))
            .filter(|service| !is_docker_service(&service.config))
            .filter_map(|service| service.runtime.pid)
            .collect();
        let process_usages = if pids.is_empty() {
            HashMap::new()
        } else {
            self.runtime
                .sample_resource_usage(&pids)
                .unwrap_or_default()
        };

        for service in &mut self.state.services {
            service.runtime.resource_usage = if is_docker_service(&service.config) {
                docker_usages.get(&service.config.id).copied()
            } else {
                service
                    .runtime
                    .pid
                    .and_then(|pid| process_usages.get(&pid).copied())
            };
        }
    }

    async fn autostart(&mut self) {
        let service_ids: Vec<String> = self
            .state
            .services
            .iter()
            .filter(|service| service.config.autostart)
            .map(|service| service.config.id.clone())
            .collect();

        for service_id in service_ids {
            if let Some(index) = self.index_of_service(&service_id) {
                if let Err(err) = self.start_service(index).await {
                    self.set_status_message(format!("autostart failed for `{service_id}`: {err}"));
                }
            }
        }
    }

    async fn shutdown(&mut self) -> Result<()> {
        let running_ids: Vec<String> = self
            .state
            .services
            .iter()
            .filter(|service| is_runtime_active(service.runtime.status))
            .map(|service| service.config.id.clone())
            .collect();
        for service_id in running_ids {
            let _ = self.runtime.stop(&service_id).await;
        }
        Ok(())
    }

    async fn start_selected_service(&mut self) -> Result<()> {
        let Some(index) = self.selected_service_index() else {
            return Ok(());
        };
        self.start_service(index).await
    }

    async fn stop_selected_service(&mut self) -> Result<()> {
        let Some(index) = self.selected_service_index() else {
            return Ok(());
        };
        self.stop_service(index).await
    }

    async fn restart_selected_service(&mut self) -> Result<()> {
        let Some(index) = self.selected_service_index() else {
            return Ok(());
        };
        self.restart_service(index).await
    }

    async fn restart_service(&mut self, index: usize) -> Result<()> {
        let config = self.state.services[index].config.clone();
        self.state.services[index].runtime.status =
            if is_runtime_active(self.state.services[index].runtime.status) {
                ServiceStatus::Stopping
            } else {
                ServiceStatus::Starting
            };
        match self.runtime.restart(config.clone()).await {
            Ok(()) => {
                self.set_status_message(format!("restarting `{}`", config.display_name()));
            }
            Err(err) => {
                self.state.services[index].runtime.status = ServiceStatus::Failed;
                self.set_status_message(err.to_string());
            }
        }
        Ok(())
    }

    async fn start_service(&mut self, index: usize) -> Result<()> {
        let config = self
            .state
            .services
            .get(index)
            .context("no selected service")?
            .config
            .clone();
        if is_runtime_active(self.state.services[index].runtime.status) {
            self.set_status_message(format!("`{}` is already running", config.display_name()));
            return Ok(());
        }

        self.state.services[index].runtime.status = ServiceStatus::Starting;
        self.state.services[index].runtime.exit_code = None;
        self.state.services[index].runtime.pid = None;
        self.state.services[index].runtime.logs.clear();
        let resolved_cwd = config.resolved_cwd(self.store.workspace_root());
        push_log(
            &mut self.state.services[index].runtime,
            LogKind::System,
            format!("cwd {}", resolved_cwd.display()),
        );
        push_log(
            &mut self.state.services[index].runtime,
            LogKind::System,
            format!("cmd {}", format_command(&config.command, &config.args)),
        );
        if !config.env.is_empty() {
            let env_summary = config
                .env
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join(" ");
            push_log(
                &mut self.state.services[index].runtime,
                LogKind::System,
                format!("env {env_summary}"),
            );
        }
        match self.runtime.start(config.clone()).await {
            Ok(()) => {
                self.set_status_message(format!("starting `{}`", config.display_name()));
            }
            Err(err) => {
                self.state.services[index].runtime.status = ServiceStatus::Failed;
                self.state.services[index].runtime.resource_usage = None;
                self.set_status_message(err.to_string());
            }
        }
        Ok(())
    }

    async fn stop_service(&mut self, index: usize) -> Result<()> {
        let config = self
            .state
            .services
            .get(index)
            .context("no selected service")?
            .config
            .clone();
        self.state.services[index].runtime.status = ServiceStatus::Stopping;
        match self.runtime.stop(&config.id).await {
            Ok(true) => {
                self.set_status_message(format!("stopping `{}`", config.display_name()));
            }
            Ok(false) => {
                self.state.services[index].runtime.status = ServiceStatus::Stopped;
                self.state.services[index].runtime.resource_usage = None;
                self.set_status_message(format!("`{}` is not running", config.display_name()));
            }
            Err(err) => {
                self.state.services[index].runtime.status = ServiceStatus::Failed;
                self.state.services[index].runtime.resource_usage = None;
                self.set_status_message(err.to_string());
            }
        }
        Ok(())
    }

    fn open_selected_service_editor(&mut self) -> Result<()> {
        let Some(index) = self.selected_service_index() else {
            bail!("no service selected");
        };
        let service = self.state.services[index].config.clone();
        self.state.right_pane = RightPaneMode::Editor(EditorState::Form(FormEditorState::new(
            FormMode::Edit { index },
            ServicePreset::Blank,
            service,
        )));
        self.state.focus = FocusPane::Details;
        Ok(())
    }

    fn open_raw_config_editor(&mut self) -> Result<()> {
        let raw = self.state.config().to_pretty_toml()?;
        self.state.right_pane =
            RightPaneMode::Editor(EditorState::Raw(RawConfigEditorState::new(raw)));
        self.state.focus = FocusPane::Details;
        Ok(())
    }

    fn save_form_editor(&mut self) -> Result<()> {
        let RightPaneMode::Editor(EditorState::Form(editor)) = &mut self.state.right_pane else {
            return Ok(());
        };

        let service = match editor.into_service_config() {
            Ok(service) => service,
            Err(err) => {
                editor.error = Some(err.to_string());
                return Ok(());
            }
        };
        let mode = editor.mode;

        let mut seen = HashSet::new();
        for (index, existing) in self.state.services.iter().enumerate() {
            if matches!(mode, FormMode::Edit { index: target } if target == index) {
                continue;
            }
            seen.insert(existing.config.id.clone());
        }
        if let Err(err) = service.validate(self.store.workspace_root(), &mut seen) {
            editor.error = Some(err.to_string());
            return Ok(());
        }

        if let FormMode::Edit { index } = mode {
            let current = &self.state.services[index];
            if is_runtime_active(current.runtime.status) && current.config.id != service.id {
                editor.error = Some("cannot rename a running service; stop it first".into());
                return Ok(());
            }
        }

        editor.error = None;
        match mode {
            FormMode::Create => {
                self.state.services.push(ServiceEntry {
                    config: service.clone(),
                    runtime: ServiceRuntime::default(),
                    log_scroll: 0,
                });
                self.state.selected_service = self.state.services.len() - 1;
            }
            FormMode::Edit { index } => {
                self.state.services[index].config = service.clone();
            }
        }
        self.persist_config()?;
        self.state.right_pane = RightPaneMode::Logs;
        self.state.focus = FocusPane::Services;
        self.set_status_message(format!("saved `{}`", service.display_name()));
        Ok(())
    }

    fn save_raw_editor(&mut self) -> Result<()> {
        let raw_text = match &self.state.right_pane {
            RightPaneMode::Editor(EditorState::Raw(raw)) => raw.buffer.as_string(),
            _ => return Ok(()),
        };
        let config = match AppConfig::parse(&raw_text) {
            Ok(config) => config,
            Err(err) => {
                if let RightPaneMode::Editor(EditorState::Raw(raw)) = &mut self.state.right_pane {
                    raw.error = Some(err.to_string());
                }
                return Ok(());
            }
        };
        if let Err(err) = config.validate(self.store.workspace_root()) {
            if let RightPaneMode::Editor(EditorState::Raw(raw)) = &mut self.state.right_pane {
                raw.error = Some(err.to_string());
            }
            return Ok(());
        }
        if let Err(err) = self.ensure_running_services_preserved(&config) {
            if let RightPaneMode::Editor(EditorState::Raw(raw)) = &mut self.state.right_pane {
                raw.error = Some(err.to_string());
            }
            return Ok(());
        }

        self.replace_config(config.clone())?;
        self.store.save(&config)?;
        if let RightPaneMode::Editor(EditorState::Raw(raw)) = &mut self.state.right_pane {
            raw.error = None;
        }
        self.state.right_pane = RightPaneMode::Logs;
        self.state.focus = FocusPane::Services;
        self.set_status_message(format!("saved {}", self.state.config_path.display()));
        Ok(())
    }

    fn ensure_running_services_preserved(&self, config: &AppConfig) -> Result<()> {
        let next_ids: HashSet<&str> = config
            .services
            .iter()
            .map(|service| service.id.as_str())
            .collect();
        for service in &self.state.services {
            if is_runtime_active(service.runtime.status)
                && !next_ids.contains(service.config.id.as_str())
            {
                bail!(
                    "running service `{}` cannot be removed or renamed; stop it first",
                    service.config.id
                );
            }
        }
        Ok(())
    }

    fn replace_config(&mut self, config: AppConfig) -> Result<()> {
        let mut runtimes: HashMap<String, (ServiceRuntime, u16)> = self
            .state
            .services
            .iter()
            .map(|service| {
                (
                    service.config.id.clone(),
                    (service.runtime.clone(), service.log_scroll),
                )
            })
            .collect();
        self.state.services = config
            .services
            .into_iter()
            .map(|service| {
                let (runtime, log_scroll) = runtimes
                    .remove(&service.id)
                    .unwrap_or((ServiceRuntime::default(), 0));
                ServiceEntry {
                    config: service,
                    runtime,
                    log_scroll,
                }
            })
            .collect();
        if self.state.services.is_empty() {
            self.state.selected_service = 0;
        } else if self.state.selected_service >= self.state.services.len() {
            self.state.selected_service = self.state.services.len() - 1;
        }
        Ok(())
    }

    fn persist_config(&mut self) -> Result<()> {
        let config = self.state.config();
        self.store.save(&config)
    }

    fn delete_selected_service(&mut self) -> Result<()> {
        let Some(index) = self.selected_service_index() else {
            return Ok(());
        };
        if is_runtime_active(self.state.services[index].runtime.status) {
            self.set_status_message("stop the service before deleting it");
            self.state.right_pane = RightPaneMode::Logs;
            self.state.focus = FocusPane::Services;
            return Ok(());
        }

        let removed = self.state.services.remove(index);
        if self.state.selected_service >= self.state.services.len()
            && !self.state.services.is_empty()
        {
            self.state.selected_service = self.state.services.len() - 1;
        }
        self.persist_config()?;
        self.state.right_pane = RightPaneMode::Logs;
        self.state.focus = FocusPane::Services;
        self.set_status_message(format!("deleted `{}`", removed.config.display_name()));
        Ok(())
    }

    fn selected_service_index(&self) -> Option<usize> {
        if self.state.services.is_empty() {
            None
        } else {
            Some(
                self.state
                    .selected_service
                    .min(self.state.services.len() - 1),
            )
        }
    }

    fn index_of_service(&self, service_id: &str) -> Option<usize> {
        self.state
            .services
            .iter()
            .position(|service| service.config.id == service_id)
    }

    fn drain_runtime_events(&mut self) {
        while let Ok(event) = self.runtime_rx.try_recv() {
            self.apply_runtime_event(event);
        }
    }

    async fn drain_api_requests(&mut self) {
        let mut commands = Vec::new();
        if let Some(api_rx) = &mut self.api_rx {
            while let Ok(command) = api_rx.try_recv() {
                commands.push(command);
            }
        }

        for command in commands {
            self.handle_api_command(command).await;
            self.drain_runtime_events();
        }
    }

    async fn handle_api_command(&mut self, command: ApiCommand) {
        match command {
            ApiCommand::List { respond_to } => {
                let _ = respond_to.send(Ok(self.service_snapshots()));
            }
            ApiCommand::Get {
                service_id,
                respond_to,
            } => {
                let _ = respond_to.send(self.api_service_snapshot(&service_id));
            }
            ApiCommand::Start {
                service_id,
                respond_to,
            } => {
                let result = self.api_start_service(&service_id).await;
                let _ = respond_to.send(result);
            }
            ApiCommand::Stop {
                service_id,
                respond_to,
            } => {
                let result = self.api_stop_service(&service_id).await;
                let _ = respond_to.send(result);
            }
            ApiCommand::Restart {
                service_id,
                respond_to,
            } => {
                let result = self.api_restart_service(&service_id).await;
                let _ = respond_to.send(result);
            }
            ApiCommand::Logs {
                service_id,
                tail,
                respond_to,
            } => {
                let _ = respond_to.send(self.api_logs(&service_id, tail));
            }
            ApiCommand::ClearLogs {
                service_id,
                respond_to,
            } => {
                let result = self.api_clear_logs(&service_id);
                let _ = respond_to.send(result);
            }
        }
    }

    fn service_snapshots(&self) -> Vec<api::ServiceSnapshot> {
        self.state
            .services
            .iter()
            .map(|service| api::service_snapshot(&service.config, &service.runtime))
            .collect()
    }

    fn api_service_snapshot(
        &self,
        service_id: &str,
    ) -> std::result::Result<api::ServiceSnapshot, ApiError> {
        let index = self
            .index_of_service(service_id)
            .ok_or_else(|| ApiError::not_found(service_id))?;
        let service = &self.state.services[index];
        Ok(api::service_snapshot(&service.config, &service.runtime))
    }

    async fn api_start_service(
        &mut self,
        service_id: &str,
    ) -> std::result::Result<ServiceActionResponse, ApiError> {
        let index = self
            .index_of_service(service_id)
            .ok_or_else(|| ApiError::not_found(service_id))?;
        self.start_service(index)
            .await
            .map_err(|err| ApiError::failed(err.to_string()))?;
        let service = self.api_service_snapshot(service_id)?;
        Ok(ServiceActionResponse {
            message: format!("start requested for `{service_id}`"),
            service,
        })
    }

    async fn api_stop_service(
        &mut self,
        service_id: &str,
    ) -> std::result::Result<ServiceActionResponse, ApiError> {
        let index = self
            .index_of_service(service_id)
            .ok_or_else(|| ApiError::not_found(service_id))?;
        self.stop_service(index)
            .await
            .map_err(|err| ApiError::failed(err.to_string()))?;
        let service = self.api_service_snapshot(service_id)?;
        Ok(ServiceActionResponse {
            message: format!("stop requested for `{service_id}`"),
            service,
        })
    }

    async fn api_restart_service(
        &mut self,
        service_id: &str,
    ) -> std::result::Result<ServiceActionResponse, ApiError> {
        let index = self
            .index_of_service(service_id)
            .ok_or_else(|| ApiError::not_found(service_id))?;
        self.restart_service(index)
            .await
            .map_err(|err| ApiError::failed(err.to_string()))?;
        let service = self.api_service_snapshot(service_id)?;
        Ok(ServiceActionResponse {
            message: format!("restart requested for `{service_id}`"),
            service,
        })
    }

    fn api_logs(
        &self,
        service_id: &str,
        tail: Option<usize>,
    ) -> std::result::Result<LogsResponse, ApiError> {
        let index = self
            .index_of_service(service_id)
            .ok_or_else(|| ApiError::not_found(service_id))?;
        let service = &self.state.services[index];
        Ok(LogsResponse {
            service: api::service_snapshot(&service.config, &service.runtime),
            logs: api::log_snapshots(&service.runtime.logs, tail),
        })
    }

    fn api_clear_logs(
        &mut self,
        service_id: &str,
    ) -> std::result::Result<ServiceActionResponse, ApiError> {
        let index = self
            .index_of_service(service_id)
            .ok_or_else(|| ApiError::not_found(service_id))?;
        self.clear_service_logs(index);
        let service = self.api_service_snapshot(service_id)?;
        Ok(ServiceActionResponse {
            message: format!("cleared logs for `{service_id}`"),
            service,
        })
    }

    fn apply_runtime_event(&mut self, event: RuntimeEvent) {
        match event {
            RuntimeEvent::Started { service_id, pid } => {
                if let Some(index) = self.index_of_service(&service_id) {
                    let display_name = self.state.services[index].config.display_name().to_string();
                    let runtime = &mut self.state.services[index].runtime;
                    runtime.status = ServiceStatus::Running;
                    runtime.pid = Some(pid);
                    runtime.exit_code = None;
                    runtime.resource_usage = None;
                    push_log(runtime, LogKind::System, format!("started pid {pid}"));
                    self.set_status_message(format!("started `{display_name}`"));
                }
            }
            RuntimeEvent::Log {
                service_id,
                stream,
                line,
            } => {
                if let Some(index) = self.index_of_service(&service_id) {
                    push_log(
                        &mut self.state.services[index].runtime,
                        match stream {
                            LogStream::Stdout => LogKind::Stdout,
                            LogStream::Stderr => LogKind::Stderr,
                        },
                        line,
                    );
                }
            }
            RuntimeEvent::Exited {
                service_id,
                exit_code,
            } => {
                if let Some(index) = self.index_of_service(&service_id) {
                    let display_name = self.state.services[index].config.display_name().to_string();
                    let runtime = &mut self.state.services[index].runtime;
                    runtime.status = ServiceStatus::Stopped;
                    runtime.pid = None;
                    runtime.exit_code = exit_code;
                    runtime.resource_usage = None;
                    let message = match exit_code {
                        Some(code) => format!("exited with {code}"),
                        None => "exited".into(),
                    };
                    push_log(runtime, LogKind::System, message.clone());
                    self.set_status_message(format!("`{display_name}` {message}"));
                }
            }
            RuntimeEvent::RuntimeError {
                service_id,
                message,
            } => {
                if let Some(index) = self.index_of_service(&service_id) {
                    let runtime = &mut self.state.services[index].runtime;
                    runtime.status = ServiceStatus::Failed;
                    runtime.resource_usage = None;
                    push_log(runtime, LogKind::System, format!("error: {message}"));
                }
                self.set_status_message(message);
            }
        }
    }
}

fn push_log(runtime: &mut ServiceRuntime, kind: LogKind, line: String) {
    runtime.logs.push(LogEntry {
        kind,
        line: sanitize_log_line(&line),
    });
    if runtime.logs.len() > MAX_LOG_LINES {
        let overflow = runtime.logs.len() - MAX_LOG_LINES;
        runtime.logs.drain(0..overflow);
    }
}

fn is_runtime_active(status: ServiceStatus) -> bool {
    matches!(
        status,
        ServiceStatus::Starting | ServiceStatus::Running | ServiceStatus::Stopping
    )
}

fn mouse_targets_logs_pane(mouse: MouseEvent) -> bool {
    let Ok((width, height)) = size() else {
        return true;
    };
    if height == 0 || mouse.row >= height.saturating_sub(1) {
        return false;
    }
    let logs_start = width.saturating_mul(30) / 100;
    mouse.column >= logs_start
}

fn format_command(command: &str, args: &[String]) -> String {
    std::iter::once(shell_quote(command))
        .chain(args.iter().map(|arg| shell_quote(arg)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".into();
    }
    let needs_quotes = value
        .chars()
        .any(|ch| ch.is_whitespace() || "'\"\\$`()[]{}*!?&;|<>".contains(ch));
    if !needs_quotes {
        return value.into();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode().context("failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).context("failed to create terminal")
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode().context("failed to disable raw mode")?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .context("failed to leave alternate screen")?;
    terminal.show_cursor().context("failed to show cursor")
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut terminate =
            signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crossterm::event::{MouseEvent, MouseEventKind};

    use super::*;
    use tempfile::tempdir;

    fn seeded_app() -> App {
        let dir = tempdir().unwrap();
        let workspace_root = dir.into_path();
        fs::create_dir_all(workspace_root.join("svc1")).unwrap();
        fs::create_dir_all(workspace_root.join("svc2")).unwrap();
        let store = ConfigStore::new(workspace_root.clone());
        let config = AppConfig {
            version: CONFIG_VERSION,
            services: vec![
                ServiceConfig {
                    id: "api".into(),
                    name: "API".into(),
                    cwd: "svc1".into(),
                    command: "echo".into(),
                    args: vec!["one".into()],
                    env: Default::default(),
                    autostart: false,
                },
                ServiceConfig {
                    id: "web".into(),
                    name: "Web".into(),
                    cwd: "svc2".into(),
                    command: "echo".into(),
                    args: vec!["two".into()],
                    env: Default::default(),
                    autostart: false,
                },
            ],
        };
        store.save(&config).unwrap();
        let (tx, rx) = mpsc::unbounded_channel();

        App {
            state: AppState {
                workspace_root: workspace_root.clone(),
                config_path: store.config_path(),
                services: config
                    .services
                    .into_iter()
                    .map(|config| ServiceEntry {
                        config,
                        runtime: ServiceRuntime::default(),
                        log_scroll: 0,
                    })
                    .collect(),
                selected_service: 0,
                focus: FocusPane::Services,
                right_pane: RightPaneMode::Logs,
                wrap_logs: false,
                status_message: None,
                status_message_expires_at: None,
            },
            store,
            runtime: RuntimeController::new(workspace_root.clone(), tx),
            runtime_rx: rx,
            api_rx: None,
            api_server: None,
            last_resource_refresh: Instant::now(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn keybindings_switch_modes_and_delete() {
        let mut app = seeded_app();
        app.handle_key(KeyEvent::from(KeyCode::Char('v')))
            .await
            .unwrap();
        assert!(matches!(
            app.state.right_pane,
            RightPaneMode::Editor(EditorState::Raw(_))
        ));

        app.handle_key(KeyEvent::from(KeyCode::Esc)).await.unwrap();
        assert!(matches!(app.state.right_pane, RightPaneMode::Logs));

        app.handle_key(KeyEvent::from(KeyCode::Char('d')))
            .await
            .unwrap();
        assert!(matches!(app.state.right_pane, RightPaneMode::ConfirmDelete));

        app.handle_key(KeyEvent::from(KeyCode::Enter))
            .await
            .unwrap();
        assert_eq!(app.state.services.len(), 1);
        assert!(matches!(app.state.right_pane, RightPaneMode::Logs));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn service_selection_moves() {
        let mut app = seeded_app();
        app.handle_key(KeyEvent::from(KeyCode::Down)).await.unwrap();
        assert_eq!(app.state.selected_service, 1);
        app.handle_key(KeyEvent::from(KeyCode::Up)).await.unwrap();
        assert_eq!(app.state.selected_service, 0);

        app.state.focus = FocusPane::Details;
        app.state.services[0].runtime.logs = (0..20)
            .map(|index| LogEntry {
                kind: LogKind::Stdout,
                line: format!("line {index}"),
            })
            .collect();
        app.handle_key(KeyEvent::from(KeyCode::Char('j')))
            .await
            .unwrap();
        assert_eq!(app.state.selected_service, 1);
        assert_eq!(app.state.services[0].log_scroll, 0);
        app.handle_key(KeyEvent::from(KeyCode::Char('k')))
            .await
            .unwrap();
        assert_eq!(app.state.selected_service, 0);
        assert_eq!(app.state.services[0].log_scroll, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mouse_scroll_moves_log_view() {
        let mut app = seeded_app();
        app.state.services[0].runtime.logs = (0..20)
            .map(|index| LogEntry {
                kind: LogKind::Stdout,
                line: format!("line {index}"),
            })
            .collect();

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 120,
            row: 0,
            modifiers: KeyModifiers::empty(),
        });
        assert_eq!(app.state.focus, FocusPane::Details);
        assert_eq!(app.state.services[0].log_scroll, 1);

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 120,
            row: 0,
            modifiers: KeyModifiers::empty(),
        });
        assert_eq!(app.state.services[0].log_scroll, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn log_shortcuts_toggle_wrap_and_clear_selected_service() {
        let mut app = seeded_app();
        app.state.services[0].runtime.logs = vec![
            LogEntry {
                kind: LogKind::Stdout,
                line: "line one".into(),
            },
            LogEntry {
                kind: LogKind::Stdout,
                line: "line two".into(),
            },
        ];
        app.state.services[0].log_scroll = 1;
        app.state.services[1].runtime.logs = vec![LogEntry {
            kind: LogKind::Stderr,
            line: "keep me".into(),
        }];

        app.handle_key(KeyEvent::from(KeyCode::Char('w')))
            .await
            .unwrap();
        assert!(app.state.wrap_logs);

        app.handle_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT))
            .await
            .unwrap();
        assert!(app.state.services[0].runtime.logs.is_empty());
        assert_eq!(app.state.services[0].log_scroll, 0);
        assert_eq!(app.state.services[1].runtime.logs.len(), 1);
    }

    #[test]
    fn status_messages_expire() {
        let mut app = seeded_app();
        app.set_status_message("starting `API`");
        assert_eq!(app.state.status_message.as_deref(), Some("starting `API`"));

        app.state.status_message_expires_at = Some(Instant::now() - Duration::from_millis(1));
        app.clear_expired_status_message();

        assert_eq!(app.state.status_message, None);
        assert_eq!(app.state.status_message_expires_at, None);
    }

    #[test]
    fn started_event_replaces_starting_message() {
        let mut app = seeded_app();
        app.set_status_message("starting `API`");

        app.apply_runtime_event(RuntimeEvent::Started {
            service_id: "api".into(),
            pid: 123,
        });

        assert_eq!(app.state.services[0].runtime.status, ServiceStatus::Running);
        assert_eq!(app.state.status_message.as_deref(), Some("started `API`"));
        assert!(app.state.status_message_expires_at.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn raw_editor_preserves_invalid_content() {
        let mut app = seeded_app();
        app.handle_key(KeyEvent::from(KeyCode::Char('v')))
            .await
            .unwrap();

        if let RightPaneMode::Editor(EditorState::Raw(raw)) = &mut app.state.right_pane {
            raw.buffer
                .set_text("version = 1\n[[services]]\nid = \"bad\"");
        }

        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
            .await
            .unwrap();

        match &app.state.right_pane {
            RightPaneMode::Editor(EditorState::Raw(raw)) => {
                assert!(raw.error.is_some());
                assert!(raw.buffer.as_string().contains("id = \"bad\""));
            }
            _ => panic!("expected raw editor to remain open"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn autostart_runs_in_config_order() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.into_path();
        fs::create_dir_all(workspace_root.join("svc1")).unwrap();
        fs::create_dir_all(workspace_root.join("svc2")).unwrap();
        let store = ConfigStore::new(workspace_root.clone());
        let config = AppConfig {
            version: CONFIG_VERSION,
            services: vec![
                ServiceConfig {
                    id: "first".into(),
                    name: String::new(),
                    cwd: "svc1".into(),
                    command: "bash".into(),
                    args: vec!["-lc".into(), "sleep 5".into()],
                    env: Default::default(),
                    autostart: true,
                },
                ServiceConfig {
                    id: "second".into(),
                    name: String::new(),
                    cwd: "svc2".into(),
                    command: "bash".into(),
                    args: vec!["-lc".into(), "sleep 5".into()],
                    env: Default::default(),
                    autostart: true,
                },
            ],
        };
        store.save(&config).unwrap();
        let mut app = App::load(workspace_root.clone()).await.unwrap();
        app.autostart().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut started = Vec::new();
        while let Ok(event) = app.runtime_rx.try_recv() {
            if let RuntimeEvent::Started { service_id, .. } = event {
                started.push(service_id);
            }
        }

        assert_eq!(started, vec!["first", "second"]);
        app.shutdown().await.unwrap();
    }
}
