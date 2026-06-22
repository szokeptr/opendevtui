use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{AppState, FocusPane, PresetPickerState, RightPaneMode, ServiceEntry};
use crate::editor::{EditorState, FormEditorState, FormField, RawConfigEditorState, ServicePreset};
use crate::runtime::{LogEntry, LogKind, ResourceUsage, ServiceStatus};

pub fn render(frame: &mut Frame, state: &AppState) -> Option<(u16, u16)> {
    frame.render_widget(Block::default().style(app_style()), frame.area());

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(frame.area());
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(outer[0]);

    render_services(frame, chunks[0], state);
    let cursor = render_right_pane(frame, chunks[1], state);
    render_footer(frame, outer[1], state);
    cursor
}

fn render_services(frame: &mut Frame, area: Rect, state: &AppState) {
    let content_width = area.width.saturating_sub(6) as usize;
    let items: Vec<ListItem> = if state.services.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "No services configured",
            muted_style(),
        )))]
    } else {
        state
            .services
            .iter()
            .map(|service| service_item(service, content_width))
            .collect()
    };

    let list = List::new(items)
        .block(
            Block::default()
                .title(Span::styled(
                    " Services ",
                    title_style(state.focus == FocusPane::Services),
                ))
                .borders(Borders::ALL)
                .border_style(border_style(state.focus == FocusPane::Services))
                .style(panel_style()),
        )
        .highlight_style(selected_style())
        .highlight_symbol("▸ ");

    let mut list_state = ListState::default();
    if !state.services.is_empty() {
        list_state.select(Some(state.selected_service));
    }
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_right_pane(frame: &mut Frame, area: Rect, state: &AppState) -> Option<(u16, u16)> {
    match &state.right_pane {
        RightPaneMode::Logs => render_logs(frame, area, state),
        RightPaneMode::PresetPicker(picker) => {
            render_preset_picker(frame, area, picker);
            None
        }
        RightPaneMode::Editor(EditorState::Form(editor)) => render_form_editor(frame, area, editor),
        RightPaneMode::Editor(EditorState::Raw(raw)) => render_raw_editor(frame, area, state, raw),
        RightPaneMode::ConfirmDelete => {
            render_delete_confirmation(frame, area, state);
            None
        }
    }
}

fn render_logs(frame: &mut Frame, area: Rect, state: &AppState) -> Option<(u16, u16)> {
    let title = if let Some(service) = state.selected_service() {
        format!(
            " Logs {} [{}] [{}] ",
            service.config.display_name(),
            status_label(service.runtime.status),
            if state.wrap_logs {
                "wrap:on"
            } else {
                "wrap:off"
            }
        )
    } else {
        " Logs ".into()
    };
    let block = Block::default()
        .title(Span::styled(
            title,
            title_style(state.focus == FocusPane::Details),
        ))
        .borders(Borders::ALL)
        .border_style(border_style(state.focus == FocusPane::Details))
        .style(panel_alt_style());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return None;
    }

    let selected = state.selected_service();
    let Some(service) = selected else {
        frame.render_widget(
            Paragraph::new(Text::from(vec![Line::from(Span::styled(
                "No services configured. Press `a` to add one.",
                muted_style(),
            ))]))
            .style(content_style())
            .wrap(Wrap { trim: false }),
            inner,
        );
        return None;
    };

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    let header = Line::from(vec![
        Span::styled("Service ", muted_style()),
        Span::styled(service.config.display_name().to_string(), emphasis_style()),
        Span::raw(" "),
        Span::styled(
            status_label(service.runtime.status),
            status_style(service.runtime.status),
        ),
        Span::raw(" "),
        Span::styled(
            format!(
                "pid {}",
                service
                    .runtime
                    .pid
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "-".into())
            ),
            muted_style(),
        ),
        Span::raw("  "),
        Span::styled("cpu ", subtle_accent_style()),
        resource_value_span(service.runtime.resource_usage.map(format_cpu)),
        Span::raw("  "),
        Span::styled("mem ", subtle_accent_style()),
        resource_value_span(service.runtime.resource_usage.map(format_memory)),
    ]);
    frame.render_widget(Paragraph::new(header).style(content_style()), sections[0]);

    if sections[1].height == 0 {
        return None;
    }

    let content = if service.runtime.logs.is_empty() {
        vec![
            Line::from(""),
            Line::from(Span::styled("No logs yet", muted_style())),
        ]
    } else {
        service
            .runtime
            .logs
            .iter()
            .map(render_log_line)
            .collect::<Vec<_>>()
    };
    let available_rows = sections[1].height as usize;
    let scroll = service.log_scroll as usize;
    let start = content
        .len()
        .saturating_sub(available_rows.saturating_add(scroll));
    let mut paragraph = Paragraph::new(Text::from(content))
        .style(content_style())
        .scroll((start as u16, 0));
    if state.wrap_logs {
        paragraph = paragraph.wrap(Wrap { trim: false });
    }
    frame.render_widget(paragraph, sections[1]);
    None
}

fn render_preset_picker(frame: &mut Frame, area: Rect, picker: &PresetPickerState) {
    let block = Block::default()
        .title(Span::styled(" Add Service ", title_style(true)))
        .borders(Borders::ALL)
        .border_style(border_style(true))
        .style(panel_alt_style());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let items: Vec<ListItem> = ServicePreset::ALL
        .iter()
        .map(|preset| {
            ListItem::new(vec![
                Line::from(Span::styled(preset.label(), emphasis_style())),
                Line::from(Span::styled(preset.description(), muted_style())),
            ])
        })
        .collect();
    let list = List::new(items)
        .highlight_style(selected_style())
        .highlight_symbol("▸ ")
        .block(
            Block::default()
                .title(Span::styled("Preset", subtle_accent_style()))
                .borders(Borders::NONE),
        );
    let mut stateful = ListState::default();
    stateful.select(Some(picker.selected));
    frame.render_stateful_widget(list, inner, &mut stateful);
}

fn render_form_editor(
    frame: &mut Frame,
    area: Rect,
    editor: &FormEditorState,
) -> Option<(u16, u16)> {
    let block = Block::default()
        .title(Span::styled(" Edit Service ", title_style(true)))
        .borders(Borders::ALL)
        .border_style(border_style(true))
        .style(panel_alt_style());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(10),
            Constraint::Min(6),
            Constraint::Length(3),
        ])
        .split(inner);

    let fields: Vec<ListItem> = FormField::ALL
        .iter()
        .map(|field| {
            let preview = editor.field_preview(*field);
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<10}", field.label()), subtle_accent_style()),
                Span::raw(" "),
                Span::styled(preview, content_style()),
            ]))
        })
        .collect();
    let list = List::new(fields)
        .highlight_style(selected_style())
        .highlight_symbol("▸ ")
        .block(
            Block::default()
                .title(Span::styled("Fields", subtle_accent_style()))
                .borders(Borders::ALL)
                .border_style(border_style(false))
                .style(panel_style()),
        );
    let mut list_state = ListState::default();
    list_state.select(Some(
        FormField::ALL
            .iter()
            .position(|field| *field == editor.selected_field)
            .unwrap_or(0),
    ));
    frame.render_stateful_widget(list, chunks[0], &mut list_state);

    let selected_title = if editor.is_editing {
        format!("{} (editing)", editor.selected_field.label())
    } else {
        editor.selected_field.label().to_string()
    };
    let editor_block = Block::default()
        .title(Span::styled(selected_title, subtle_accent_style()))
        .borders(Borders::ALL)
        .border_style(border_style(editor.is_editing))
        .style(panel_style());
    let editor_inner = editor_block.inner(chunks[1]);
    frame.render_widget(editor_block, chunks[1]);
    let field_text = match editor.selected_field {
        FormField::Autostart => Text::from(vec![
            Line::from(vec![
                Span::styled("Current value: ", muted_style()),
                Span::styled(
                    if editor.autostart { "true" } else { "false" },
                    if editor.autostart {
                        status_style(ServiceStatus::Running)
                    } else {
                        status_style(ServiceStatus::Stopped)
                    },
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "Press Enter or Space to toggle.",
                muted_style(),
            )),
        ]),
        _ => Text::from(
            editor
                .active_buffer()
                .map(render_buffer_lines)
                .unwrap_or_else(|| vec![Line::from("")]),
        ),
    };
    frame.render_widget(
        Paragraph::new(field_text)
            .style(content_style())
            .wrap(Wrap { trim: false }),
        editor_inner,
    );

    let help = vec![
        Line::from("Enter/i edit field | Esc cancel | Ctrl+S save"),
        Line::from("Args and env use one line per value or KEY=VALUE entry"),
    ];
    frame.render_widget(
        Paragraph::new(Text::from(help)).style(muted_style()).block(
            Block::default()
                .title(Span::styled("Help", subtle_accent_style()))
                .borders(Borders::ALL)
                .border_style(border_style(false))
                .style(panel_style()),
        ),
        chunks[2],
    );

    if let Some(message) = editor.error.as_deref() {
        render_status(frame, area, message);
    }

    if editor.is_editing && editor.selected_field != FormField::Autostart {
        let buffer = editor.active_buffer()?;
        return cursor_for_buffer(editor_inner, buffer, false);
    }
    None
}

fn render_raw_editor(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    raw: &RawConfigEditorState,
) -> Option<(u16, u16)> {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(3)])
        .split(area);

    let block = Block::default()
        .title(Span::styled(
            format!(" Raw Config {} ", state.config_path.display()),
            title_style(true),
        ))
        .borders(Borders::ALL)
        .border_style(border_style(true))
        .style(panel_alt_style());
    let inner = block.inner(chunks[0]);
    frame.render_widget(block, chunks[0]);

    let rendered = render_numbered_buffer(raw);
    frame.render_widget(
        Paragraph::new(rendered)
            .style(content_style())
            .wrap(Wrap { trim: false }),
        inner,
    );
    frame.render_widget(
        Paragraph::new(Text::from(vec![
            Line::from(Span::styled(
                "Ctrl+S save | Esc discard changes",
                muted_style(),
            )),
            Line::from(Span::styled(
                "The editor stays open if TOML parse or validation fails.",
                muted_style(),
            )),
        ]))
        .block(
            Block::default()
                .title(Span::styled("Help", subtle_accent_style()))
                .borders(Borders::ALL)
                .border_style(border_style(false))
                .style(panel_style()),
        ),
        chunks[1],
    );

    if let Some(message) = raw.error.as_deref() {
        render_status(frame, area, message);
    }

    cursor_for_buffer(inner, &raw.buffer, true)
}

fn render_delete_confirmation(frame: &mut Frame, area: Rect, state: &AppState) {
    let message = state
        .selected_service()
        .map(|service| format!("Delete `{}` from config?", service.config.display_name()))
        .unwrap_or_else(|| "Delete selected service?".into());
    let popup = centered_rect(60, 30, area);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(Text::from(vec![
            Line::from(Span::styled(message, emphasis_style())),
            Line::from(""),
            Line::from(Span::styled(
                "Enter/y confirm | n/Esc cancel",
                muted_style(),
            )),
        ]))
        .block(
            Block::default()
                .title(Span::styled("Confirm Delete", popup_title_style()))
                .borders(Borders::ALL)
                .border_style(popup_border_style())
                .style(popup_style()),
        )
        .wrap(Wrap { trim: false }),
        popup,
    );
}

fn render_status(frame: &mut Frame, area: Rect, message: &str) {
    let popup = centered_rect(80, 20, area);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(Span::styled(message, content_style()))
            .block(
                Block::default()
                    .title(Span::styled("Message", popup_title_style()))
                    .borders(Borders::ALL)
                    .border_style(popup_border_style())
                    .style(popup_style()),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn service_item(service: &ServiceEntry, content_width: usize) -> ListItem<'static> {
    let metrics = format_metrics_compact(service.runtime.resource_usage);
    let spacer_width = content_width
        .saturating_sub(service.config.id.chars().count())
        .saturating_sub(metrics.chars().count())
        .max(1);

    ListItem::new(vec![
        Line::from(vec![
            Span::styled("● ", status_style(service.runtime.status)),
            Span::styled(service.config.display_name().to_string(), emphasis_style()),
        ]),
        Line::from(vec![
            Span::styled(service.config.id.to_string(), subtle_accent_style()),
            Span::raw(" ".repeat(spacer_width)),
            Span::styled(metrics, muted_style()),
        ]),
    ])
}

fn render_footer(frame: &mut Frame, area: Rect, state: &AppState) {
    let mode = match &state.right_pane {
        RightPaneMode::Logs => "LOGS",
        RightPaneMode::PresetPicker(_) => "ADD",
        RightPaneMode::Editor(EditorState::Form(_)) => "FORM",
        RightPaneMode::Editor(EditorState::Raw(_)) => "RAW",
        RightPaneMode::ConfirmDelete => "CONFIRM",
    };
    let focus = match state.focus {
        FocusPane::Services => "SERVICES",
        FocusPane::Details => "DETAILS",
    };
    let hints = shortcut_hints(state);
    let mut spans = vec![
        Span::styled(format!(" {} ", mode), badge_style(PALETTE.accent)),
        Span::raw(" "),
        Span::styled(
            format!("{} ", focus),
            Style::default()
                .fg(PALETTE.sky)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ];
    if let Some(message) = &state.status_message {
        spans.push(Span::styled(message.to_string(), content_style()));
        spans.push(Span::styled("  |  ", muted_style()));
    }
    spans.push(Span::styled(hints, muted_style()));
    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line).style(footer_style()), area);
}

fn shortcut_hints(state: &AppState) -> String {
    match &state.right_pane {
        RightPaneMode::Logs => logs_shortcut_hints(state),
        RightPaneMode::PresetPicker(_) => "j/k choose  Enter add preset  Esc cancel".into(),
        RightPaneMode::Editor(EditorState::Form(editor)) if editor.is_editing => {
            let done = if editor.selected_field.is_multiline() {
                "Esc done"
            } else {
                "Enter done"
            };
            format!("{done}  type to edit")
        }
        RightPaneMode::Editor(EditorState::Form(editor)) => {
            let edit = if editor.selected_field == FormField::Autostart {
                "Enter/Space toggle"
            } else {
                "Enter/i edit"
            };
            format!("j/k field  {edit}  Ctrl+S save  Esc cancel")
        }
        RightPaneMode::Editor(EditorState::Raw(_)) => "Ctrl+S save  Esc discard".into(),
        RightPaneMode::ConfirmDelete => "Enter/y delete  n/Esc cancel".into(),
    }
}

fn logs_shortcut_hints(state: &AppState) -> String {
    if state.services.is_empty() {
        return "a add service  v raw config  q quit".into();
    }

    match state.focus {
        FocusPane::Services => {
            "j/k move  Tab logs  s start  x stop  r restart  e edit  a add  d delete  v raw  q quit"
                .into()
        }
        FocusPane::Details => {
            "j/k service  Up/Down scroll  PgUp/PgDn page  w wrap  Shift+C clear  Tab services  q quit"
                .into()
        }
    }
}

fn status_label(status: ServiceStatus) -> &'static str {
    match status {
        ServiceStatus::Stopped => "stopped",
        ServiceStatus::Starting => "starting",
        ServiceStatus::Running => "running",
        ServiceStatus::Stopping => "stopping",
        ServiceStatus::Failed => "failed",
    }
}

fn border_style(active: bool) -> Style {
    if active {
        Style::default()
            .fg(PALETTE.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(PALETTE.panel_high)
    }
}

fn render_buffer_lines(buffer: &crate::editor::TextBuffer) -> Vec<Line<'static>> {
    buffer
        .lines()
        .iter()
        .map(|line| Line::from(Span::styled(line.clone(), content_style())))
        .collect()
}

fn render_numbered_buffer(raw: &RawConfigEditorState) -> Text<'static> {
    let lines: Vec<Line> = raw
        .buffer
        .lines()
        .iter()
        .enumerate()
        .map(|(index, line)| {
            Line::from(vec![
                Span::styled(format!("{:>3} ", index + 1), subtle_accent_style()),
                Span::styled(line.clone(), content_style()),
            ])
        })
        .collect();
    Text::from(lines)
}

fn cursor_for_buffer(
    area: Rect,
    buffer: &crate::editor::TextBuffer,
    numbered: bool,
) -> Option<(u16, u16)> {
    if area.width < 2 || area.height < 1 {
        return None;
    }
    let height = area.height as usize;
    let row = buffer.cursor_row;
    let start_row = row.saturating_sub(height.saturating_sub(1));
    let visible_row = row.saturating_sub(start_row) as u16;
    if visible_row >= area.height {
        return None;
    }

    let prefix = if numbered { 4 } else { 0 };
    let x = area
        .x
        .saturating_add(prefix)
        .saturating_add(buffer.cursor_col as u16)
        .min(area.right().saturating_sub(1));
    let y = area.y.saturating_add(visible_row);
    Some((x, y))
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn render_log_line(entry: &LogEntry) -> Line<'static> {
    match entry.kind {
        LogKind::Stdout => Line::from(Span::styled(entry.line.clone(), content_style())),
        LogKind::Stderr => Line::from(Span::styled(
            entry.line.clone(),
            Style::default().fg(PALETTE.peach),
        )),
        LogKind::System => Line::from(vec![
            Span::styled("› ", Style::default().fg(PALETTE.sky)),
            Span::styled(entry.line.clone(), muted_style()),
        ]),
    }
}

fn resource_value_span(value: Option<String>) -> Span<'static> {
    match value {
        Some(value) => Span::styled(value, content_style()),
        None => Span::styled("--", muted_style()),
    }
}

fn format_cpu(usage: ResourceUsage) -> String {
    format!("{:.1}%", usage.cpu_percent)
}

fn format_memory(usage: ResourceUsage) -> String {
    let mib = usage.memory_kib as f64 / 1024.0;
    if mib >= 1024.0 {
        format!("{:.1} GiB", mib / 1024.0)
    } else {
        format!("{:.1} MiB", mib)
    }
}

fn format_metrics_compact(usage: Option<ResourceUsage>) -> String {
    match usage {
        Some(usage) => format!("{} {}", format_cpu(usage), format_memory_compact(usage)),
        None => "-- --".into(),
    }
}

fn format_memory_compact(usage: ResourceUsage) -> String {
    let mib = usage.memory_kib as f64 / 1024.0;
    if mib >= 1024.0 {
        format!("{:.1}G", mib / 1024.0)
    } else if mib >= 10.0 {
        format!("{:.0}M", mib)
    } else if mib >= 1.0 {
        format!("{:.1}M", mib)
    } else {
        format!("{}K", usage.memory_kib)
    }
}

fn app_style() -> Style {
    Style::default().bg(PALETTE.bg).fg(PALETTE.text)
}

fn panel_style() -> Style {
    Style::default().bg(PALETTE.panel).fg(PALETTE.text)
}

fn panel_alt_style() -> Style {
    Style::default().bg(PALETTE.panel_alt).fg(PALETTE.text)
}

fn popup_style() -> Style {
    Style::default().bg(PALETTE.popup).fg(PALETTE.text)
}

fn footer_style() -> Style {
    Style::default().bg(PALETTE.footer).fg(PALETTE.text)
}

fn title_style(active: bool) -> Style {
    if active {
        Style::default()
            .fg(PALETTE.accent)
            .bg(PALETTE.title_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(PALETTE.muted)
            .bg(PALETTE.title_bg)
            .add_modifier(Modifier::BOLD)
    }
}

fn popup_title_style() -> Style {
    Style::default()
        .fg(PALETTE.sun)
        .bg(PALETTE.title_bg)
        .add_modifier(Modifier::BOLD)
}

fn popup_border_style() -> Style {
    Style::default()
        .fg(PALETTE.sun)
        .add_modifier(Modifier::BOLD)
}

fn emphasis_style() -> Style {
    Style::default()
        .fg(PALETTE.text)
        .add_modifier(Modifier::BOLD)
}

fn content_style() -> Style {
    Style::default().fg(PALETTE.text)
}

fn muted_style() -> Style {
    Style::default().fg(PALETTE.muted)
}

fn subtle_accent_style() -> Style {
    Style::default()
        .fg(PALETTE.sky)
        .add_modifier(Modifier::BOLD)
}

fn selected_style() -> Style {
    Style::default()
        .bg(PALETTE.selection)
        .fg(PALETTE.text)
        .add_modifier(Modifier::BOLD)
}

fn badge_style(color: Color) -> Style {
    Style::default()
        .bg(color)
        .fg(PALETTE.bg)
        .add_modifier(Modifier::BOLD)
}

fn status_style(status: ServiceStatus) -> Style {
    Style::default()
        .fg(status_color(status))
        .add_modifier(Modifier::BOLD)
}

fn status_color(status: ServiceStatus) -> Color {
    match status {
        ServiceStatus::Stopped => PALETTE.stone,
        ServiceStatus::Starting => PALETTE.sun,
        ServiceStatus::Running => PALETTE.mint,
        ServiceStatus::Stopping => PALETTE.peach,
        ServiceStatus::Failed => PALETTE.coral,
    }
}

struct Palette {
    bg: Color,
    panel: Color,
    panel_alt: Color,
    panel_high: Color,
    popup: Color,
    footer: Color,
    title_bg: Color,
    text: Color,
    muted: Color,
    accent: Color,
    selection: Color,
    sky: Color,
    mint: Color,
    peach: Color,
    coral: Color,
    sun: Color,
    stone: Color,
}

const PALETTE: Palette = Palette {
    bg: Color::Rgb(14, 18, 24),
    panel: Color::Rgb(22, 28, 36),
    panel_alt: Color::Rgb(18, 24, 32),
    panel_high: Color::Rgb(54, 69, 87),
    popup: Color::Rgb(28, 20, 18),
    footer: Color::Rgb(16, 22, 30),
    title_bg: Color::Rgb(28, 36, 48),
    text: Color::Rgb(230, 235, 240),
    muted: Color::Rgb(140, 154, 170),
    accent: Color::Rgb(255, 183, 77),
    selection: Color::Rgb(46, 60, 78),
    sky: Color::Rgb(112, 188, 255),
    mint: Color::Rgb(109, 224, 164),
    peach: Color::Rgb(255, 166, 117),
    coral: Color::Rgb(255, 107, 107),
    sun: Color::Rgb(255, 210, 92),
    stone: Color::Rgb(161, 170, 181),
};
