//! The help overlay, which is the only place the three keys are written down.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::theme::Theme;

pub fn draw(frame: &mut Frame, area: Rect, theme: &Theme) {
    let body = [
        "",
        "  pippipit owns these four keys only",
        "",
        "    r    refresh all sources",
        "    p    hide the SSID and the address, for a screenshot",
        "    ?    this help",
        "    q    quit",
        "",
        "  Volume / playback / workspace belong to Hyprland keybinds",
        "",
        "    ALT + 1..9        switch workspace",
        "    XF86AudioMute     toggle mute",
        "    XF86AudioPlay     play / pause",
        "",
        "  Everything else is a click: volume, mute, seek, workspace, power",
        "",
        "  Esc or ? to close",
        "",
    ];
    let w = 60.min(area.width.saturating_sub(4));
    let h = (body.len() as u16 + 2).min(area.height.saturating_sub(2));
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(" Help ", Style::default().fg(theme.accent)));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(
            body.iter()
                .map(|l| Line::styled(*l, Style::default().fg(theme.text)))
                .collect::<Vec<_>>(),
        ),
        inner,
    );
}
