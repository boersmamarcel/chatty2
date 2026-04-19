use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::APP_VERSION;
use crate::engine::ChatEngine;
use crate::ui::theme;

pub fn render_status_bar(frame: &mut Frame, area: Rect, engine: &ChatEngine) {
    let model_name = &engine.model_config.name;
    let cwd_max_len =
        usize::from(
            area.width
                .saturating_sub(if engine.git_branch.is_some() { 40 } else { 24 }),
        )
        .clamp(12, 48);
    let working_dir = truncate_middle(&engine.current_working_directory(), cwd_max_len);

    let mut spans = vec![
        Span::styled(format!(" {} ", model_name), theme::text_bold()),
        Span::styled(" │ ", theme::muted()),
        Span::styled(format!("v{}", APP_VERSION), theme::text_subtle()),
        Span::styled(" │ ", theme::muted()),
        Span::styled("cwd ", theme::text_subtle()),
        Span::styled(working_dir, theme::text()),
    ];

    if let Some(branch) = engine.git_branch.as_deref() {
        spans.push(Span::styled(" │ ", theme::muted()));
        spans.push(Span::styled("git ", theme::text_subtle()));
        spans.push(Span::styled(
            truncate_middle(branch, 18),
            theme::tool_bold(),
        ));
    }

    spans.push(Span::styled(" │ ", theme::muted()));

    // Token count
    if engine.total_input_tokens > 0 || engine.total_output_tokens > 0 {
        spans.push(Span::styled(
            format!(
                "{}↑ {}↓",
                format_tokens(engine.total_input_tokens),
                format_tokens(engine.total_output_tokens),
            ),
            theme::muted(),
        ));
        spans.push(Span::styled(" │ ", theme::muted()));
    }

    // Status indicator
    if !engine.is_ready {
        spans.push(Span::styled("● initializing…", theme::accent()));
    } else if engine.is_streaming {
        spans.push(Span::styled("● streaming", theme::warning()));
    } else {
        spans.push(Span::styled("● ready", theme::success()));
    }

    // Right-aligned scroll indicator (only when not pinned)
    if !engine.pinned_to_bottom {
        let indicator = format!(" ↑ {} lines", engine.scroll_offset);
        let left_len: u16 = spans.iter().map(|s| s.content.chars().count() as u16).sum();
        let ind_len = indicator.len() as u16;
        if left_len + ind_len < area.width {
            let padding = area.width.saturating_sub(left_len + ind_len);
            spans.push(Span::raw(" ".repeat(padding as usize)));
            spans.push(Span::styled(indicator, theme::accent()));
        }
    }

    let status_line = Paragraph::new(Line::from(spans)).style(theme::status_bar());

    frame.render_widget(status_line, area);
}

fn format_tokens(count: u32) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

fn truncate_middle(value: &str, max_len: usize) -> String {
    if value.chars().count() <= max_len || max_len <= 3 {
        return value.to_string();
    }

    let keep = (max_len - 1) / 2;
    let prefix: String = value.chars().take(keep).collect();
    let suffix: String = value
        .chars()
        .rev()
        .take(max_len - keep - 1)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{prefix}…{suffix}")
}
