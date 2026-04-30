use std::collections::BTreeMap;

use anyhow::{bail, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::ServiceConfig;

#[derive(Debug, Clone)]
pub struct TextBuffer {
    lines: Vec<String>,
    pub cursor_row: usize,
    pub cursor_col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormMode {
    Create,
    Edit { index: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    Id,
    Name,
    Cwd,
    Command,
    Args,
    Env,
    Autostart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServicePreset {
    Blank,
    Npm,
    Docker,
    Bash,
}

#[derive(Debug, Clone)]
pub struct FormEditorState {
    pub mode: FormMode,
    pub preset: ServicePreset,
    pub selected_field: FormField,
    pub is_editing: bool,
    pub id: TextBuffer,
    pub name: TextBuffer,
    pub cwd: TextBuffer,
    pub command: TextBuffer,
    pub args: TextBuffer,
    pub env: TextBuffer,
    pub autostart: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RawConfigEditorState {
    pub buffer: TextBuffer,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum EditorState {
    Form(FormEditorState),
    Raw(RawConfigEditorState),
}

impl TextBuffer {
    pub fn new() -> Self {
        Self::from_string("")
    }

    pub fn from_string(input: &str) -> Self {
        let mut lines: Vec<String> = input.lines().map(ToOwned::to_owned).collect();
        if input.ends_with('\n') {
            lines.push(String::new());
        }
        if lines.is_empty() {
            lines.push(String::new());
        }
        Self {
            lines,
            cursor_row: 0,
            cursor_col: 0,
        }
    }

    pub fn as_string(&self) -> String {
        self.lines.join("\n")
    }

    pub fn current_line(&self) -> &str {
        &self.lines[self.cursor_row]
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn set_text(&mut self, text: &str) {
        *self = Self::from_string(text);
    }

    pub fn handle_key(&mut self, key: KeyEvent, allow_newline: bool) -> bool {
        match key.code {
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.insert_char(c);
                true
            }
            KeyCode::Tab => {
                self.insert_char('\t');
                true
            }
            KeyCode::Enter if allow_newline => {
                self.insert_newline();
                true
            }
            KeyCode::Backspace => {
                self.backspace();
                true
            }
            KeyCode::Delete => {
                self.delete();
                true
            }
            KeyCode::Left => {
                self.move_left();
                true
            }
            KeyCode::Right => {
                self.move_right();
                true
            }
            KeyCode::Up if allow_newline => {
                self.move_up();
                true
            }
            KeyCode::Down if allow_newline => {
                self.move_down();
                true
            }
            KeyCode::Home => {
                self.cursor_col = 0;
                true
            }
            KeyCode::End => {
                self.cursor_col = self.current_line().len();
                true
            }
            _ => false,
        }
    }

    fn insert_char(&mut self, c: char) {
        let line = &mut self.lines[self.cursor_row];
        let col = self.cursor_col.min(line.len());
        line.insert(col, c);
        self.cursor_col = col + c.len_utf8();
    }

    fn insert_newline(&mut self) {
        let current = &mut self.lines[self.cursor_row];
        let remainder = current.split_off(self.cursor_col.min(current.len()));
        self.cursor_row += 1;
        self.cursor_col = 0;
        self.lines.insert(self.cursor_row, remainder);
    }

    fn backspace(&mut self) {
        if self.cursor_col > 0 {
            let line = &mut self.lines[self.cursor_row];
            let remove_at = self.cursor_col - 1;
            line.remove(remove_at);
            self.cursor_col = remove_at;
        } else if self.cursor_row > 0 {
            let current = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            let new_col = self.lines[self.cursor_row].len();
            self.lines[self.cursor_row].push_str(&current);
            self.cursor_col = new_col;
        }
    }

    fn delete(&mut self) {
        let line_len = self.lines[self.cursor_row].len();
        if self.cursor_col < line_len {
            self.lines[self.cursor_row].remove(self.cursor_col);
        } else if self.cursor_row + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_row + 1);
            self.lines[self.cursor_row].push_str(&next);
        }
    }

    fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
        }
    }

    fn move_right(&mut self) {
        let line_len = self.lines[self.cursor_row].len();
        if self.cursor_col < line_len {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].len());
        }
    }

    fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].len());
        }
    }
}

impl FormField {
    pub const ALL: [FormField; 7] = [
        FormField::Id,
        FormField::Name,
        FormField::Cwd,
        FormField::Command,
        FormField::Args,
        FormField::Env,
        FormField::Autostart,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FormField::Id => "ID",
            FormField::Name => "Name",
            FormField::Cwd => "Cwd",
            FormField::Command => "Command",
            FormField::Args => "Args",
            FormField::Env => "Env",
            FormField::Autostart => "Autostart",
        }
    }

    pub fn is_multiline(self) -> bool {
        matches!(self, FormField::Args | FormField::Env)
    }

    fn prev(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|field| *field == self)
            .unwrap_or(0);
        if index == 0 {
            Self::ALL[Self::ALL.len() - 1]
        } else {
            Self::ALL[index - 1]
        }
    }

    fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|field| *field == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

impl ServicePreset {
    pub const ALL: [ServicePreset; 4] = [
        ServicePreset::Blank,
        ServicePreset::Npm,
        ServicePreset::Docker,
        ServicePreset::Bash,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ServicePreset::Blank => "Blank service",
            ServicePreset::Npm => "npm run dev",
            ServicePreset::Docker => "docker compose up",
            ServicePreset::Bash => "bash script",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            ServicePreset::Blank => "Start from empty fields",
            ServicePreset::Npm => "Prefill command and args for a typical frontend dev server",
            ServicePreset::Docker => "Prefill a compose-based service",
            ServicePreset::Bash => "Prefill a bash command wrapper",
        }
    }

    pub fn into_service(self) -> ServiceConfig {
        match self {
            ServicePreset::Blank => ServiceConfig {
                id: String::new(),
                name: String::new(),
                cwd: ".".into(),
                command: String::new(),
                args: Vec::new(),
                env: BTreeMap::new(),
                autostart: false,
            },
            ServicePreset::Npm => ServiceConfig {
                id: String::new(),
                name: "Frontend".into(),
                cwd: ".".into(),
                command: "npm".into(),
                args: vec!["run".into(), "dev".into()],
                env: BTreeMap::new(),
                autostart: false,
            },
            ServicePreset::Docker => ServiceConfig {
                id: String::new(),
                name: "Compose".into(),
                cwd: ".".into(),
                command: "docker".into(),
                args: vec!["compose".into(), "up".into()],
                env: BTreeMap::new(),
                autostart: false,
            },
            ServicePreset::Bash => ServiceConfig {
                id: String::new(),
                name: "Script".into(),
                cwd: ".".into(),
                command: "bash".into(),
                args: vec!["-lc".into(), "./script.sh".into()],
                env: BTreeMap::new(),
                autostart: false,
            },
        }
    }
}

impl FormEditorState {
    pub fn new(mode: FormMode, preset: ServicePreset, service: ServiceConfig) -> Self {
        Self {
            mode,
            preset,
            selected_field: FormField::Id,
            is_editing: false,
            id: TextBuffer::from_string(&service.id),
            name: TextBuffer::from_string(&service.name),
            cwd: TextBuffer::from_string(&service.cwd),
            command: TextBuffer::from_string(&service.command),
            args: TextBuffer::from_string(&service.args.join("\n")),
            env: TextBuffer::from_string(&format_env(&service.env)),
            autostart: service.autostart,
            error: None,
        }
    }

    pub fn start_editing(&mut self) {
        if self.selected_field != FormField::Autostart {
            self.is_editing = true;
        }
    }

    pub fn stop_editing(&mut self) {
        self.is_editing = false;
    }

    pub fn move_prev(&mut self) {
        self.selected_field = self.selected_field.prev();
    }

    pub fn move_next(&mut self) {
        self.selected_field = self.selected_field.next();
    }

    pub fn toggle_autostart(&mut self) {
        self.autostart = !self.autostart;
    }

    pub fn active_buffer_mut(&mut self) -> Option<&mut TextBuffer> {
        match self.selected_field {
            FormField::Id => Some(&mut self.id),
            FormField::Name => Some(&mut self.name),
            FormField::Cwd => Some(&mut self.cwd),
            FormField::Command => Some(&mut self.command),
            FormField::Args => Some(&mut self.args),
            FormField::Env => Some(&mut self.env),
            FormField::Autostart => None,
        }
    }

    pub fn active_buffer(&self) -> Option<&TextBuffer> {
        match self.selected_field {
            FormField::Id => Some(&self.id),
            FormField::Name => Some(&self.name),
            FormField::Cwd => Some(&self.cwd),
            FormField::Command => Some(&self.command),
            FormField::Args => Some(&self.args),
            FormField::Env => Some(&self.env),
            FormField::Autostart => None,
        }
    }

    pub fn field_preview(&self, field: FormField) -> String {
        match field {
            FormField::Id => self.id.as_string(),
            FormField::Name => self.name.as_string(),
            FormField::Cwd => self.cwd.as_string(),
            FormField::Command => self.command.as_string(),
            FormField::Args => join_preview(self.args.lines()),
            FormField::Env => join_preview(self.env.lines()),
            FormField::Autostart => {
                if self.autostart {
                    "true".into()
                } else {
                    "false".into()
                }
            }
        }
    }

    pub fn into_service_config(&self) -> Result<ServiceConfig> {
        let env = parse_env_lines(self.env.lines())?;
        Ok(ServiceConfig {
            id: self.id.as_string().trim().to_string(),
            name: self.name.as_string().trim().to_string(),
            cwd: self.cwd.as_string().trim().to_string(),
            command: self.command.as_string().trim().to_string(),
            args: self
                .args
                .lines()
                .iter()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
            env,
            autostart: self.autostart,
        })
    }
}

impl RawConfigEditorState {
    pub fn new(text: String) -> Self {
        Self {
            buffer: TextBuffer::from_string(&text),
            error: None,
        }
    }
}

pub fn parse_env_lines(lines: &[String]) -> Result<BTreeMap<String, String>> {
    let mut env = BTreeMap::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            bail!("invalid env line `{trimmed}`; expected KEY=VALUE");
        };
        let key = key.trim();
        if key.is_empty() {
            bail!("env key cannot be empty");
        }
        env.insert(key.to_string(), value.to_string());
    }
    Ok(env)
}

fn format_env(env: &BTreeMap<String, String>) -> String {
    env.iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn join_preview(lines: &[String]) -> String {
    let values: Vec<&str> = lines
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect();
    if values.is_empty() {
        "-".into()
    } else {
        values.join(" | ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_buffer_basic_editing() {
        let mut buffer = TextBuffer::from_string("ab");
        buffer.handle_key(KeyEvent::from(KeyCode::Char('c')), false);
        assert_eq!(buffer.as_string(), "cab");
        buffer.handle_key(KeyEvent::from(KeyCode::Right), false);
        buffer.handle_key(KeyEvent::from(KeyCode::Right), false);
        buffer.handle_key(KeyEvent::from(KeyCode::Backspace), false);
        assert_eq!(buffer.as_string(), "ca");
    }

    #[test]
    fn form_editor_parses_env_and_args() {
        let service = ServicePreset::Npm.into_service();
        let mut editor = FormEditorState::new(FormMode::Create, ServicePreset::Npm, service);
        editor.id.set_text("frontend");
        editor.env.set_text("PORT=3000\nHOST=0.0.0.0");
        let service = editor.into_service_config().unwrap();

        assert_eq!(service.args, vec!["run", "dev"]);
        assert_eq!(service.env["PORT"], "3000");
        assert_eq!(service.id, "frontend");
    }

    #[test]
    fn invalid_env_line_errors() {
        let err = parse_env_lines(&[String::from("BROKEN")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("expected KEY=VALUE"));
    }
}
