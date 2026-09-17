//! Writing straight into the buffer, for the parts no widget owns.
//!
//! The frame is stroked by hand rather than by two `Block`s, so the divider between
//! the panes is a single rule with a cross in it.

use ratatui::style::Style;
use ratatui::text::Line;

use super::theme::Theme;

pub fn set(buf: &mut ratatui::buffer::Buffer, x: u16, y: u16, symbol: &str, style: Style) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_symbol(symbol).set_style(style);
    }
}

pub fn write_str(buf: &mut ratatui::buffer::Buffer, x: u16, y: u16, text: &str, style: Style) {
    for (i, ch) in text.chars().enumerate() {
        set(buf, x + i as u16, y, &ch.to_string(), style);
    }
}

/// The divider drawn on the same row in both panes; this is what forms the cross.
pub fn separator<'a>(width: u16, theme: &Theme) -> Line<'a> {
    Line::styled(
        format!("  {}", "─".repeat(width.saturating_sub(4) as usize)),
        Style::default().fg(theme.border),
    )
}
