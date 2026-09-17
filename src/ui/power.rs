//! The power row along the bottom, and the status line sharing it.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::sources::power::PowerAction;
use crate::store::UiState;

use super::hit::Action;
use super::slot::{DrawCtx, Measure, Rendered, Slot, priority};
use super::theme::{Part, Theme};
use crate::util::text::width;

/// Columns between one button and the next.
const GAP: usize = 3;

/// The one column of the gap that belongs to neither neighbour.
///
/// In the transport row the whole gap belongs to the button on its left, because a
/// one-column glyph is a hard target. Here the two rightmost buttons suspend and power
/// off the machine, so their areas stop a column short of the next one instead of
/// running into it.
const DEAD: u16 = 1;

/// Columns one button takes.
///
/// The widest of the three wins, so they stay on an even stride whichever font is in
/// use: one column each with a Nerd Font, and the width of `[ Screen Off ]` without.
fn button_width(theme: &Theme) -> usize {
    [
        PowerAction::ScreenOff,
        PowerAction::Suspend,
        PowerAction::PowerOff,
    ]
    .iter()
    .map(|action| width(theme.icon_power_action(*action)))
    .max()
    .unwrap_or(1)
}

/// The power row, and the status line sharing it. **Mouse only** - it has no keys.
pub struct PowerSlot<'a> {
    pub ui: &'a UiState,
    /// A refused workspace switch has nowhere else to go, so it borrows this row.
    pub switch_error: Option<&'a str>,
}

impl Slot for PowerSlot<'_> {
    fn part(&self) -> Part {
        Part::Power
    }

    /// One row, and the first thing a short layout gives up: every other block says
    /// something the keyboard cannot ask for.
    fn measure(&self, _width: u16) -> Measure {
        Measure::fixed(1, priority::POWER)
    }

    fn render(&self, ctx: &DrawCtx, area: Rect) -> Rendered<'static> {
        let theme = ctx.theme;
        let mut hits: Vec<(Rect, Action)> = Vec::new();
        let buttons = [
            PowerAction::ScreenOff,
            PowerAction::Suspend,
            PowerAction::PowerOff,
        ];

        // While an action is pending, the buttons go dead.
        // That also blocks a double press from queueing a second suspend or poweroff.
        let armed = self.ui.power_running.is_none();

        let cell = button_width(theme);
        let mut spans = vec![Span::raw("  ")];
        let mut x = area.x + 2;
        for action in buttons {
            let glyph = theme.icon_power_action(action);
            if armed {
                hits.push((
                    Rect::new(x, area.y, (cell + GAP) as u16 - DEAD, 1),
                    Action::Power(action),
                ));
            }
            // Padded out to the cell so a narrow glyph still lines up with its
            // neighbours, and so the hint on the right does not move when the font does.
            spans.push(Span::styled(
                format!("{glyph}{}", " ".repeat(cell.saturating_sub(width(glyph)))),
                Style::default().fg(if armed { theme.dim } else { theme.border }),
            ));
            spans.push(Span::raw(" ".repeat(GAP)));
            x += (cell + GAP) as u16;
        }

        // The hint at the far right.
        // During the delay it looks like nothing happened, so this is always shown.
        // A failed click has nowhere else to go: the strip is a fixed height and the
        // status line is the one place a transient message can appear.
        let error = self
            .ui
            .power_error
            .as_ref()
            .map(|err| format!("power: {err}"))
            .or_else(|| self.switch_error.map(str::to_string));
        let hint = match (&error, self.ui.power_running) {
            (Some(err), _) => err.clone(),
            (None, Some(action)) => format!("{}…", action.label()),
            (None, None) => "r  ?  q".to_string(),
        };
        let used: usize = spans.iter().map(|s| width(&s.content)).sum();
        let gap = (area.width as usize).saturating_sub(used + hint.chars().count() + 2);
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(
            hint.clone(),
            Style::default().fg(if error.is_some() {
                theme.crit
            } else if self.ui.power_running.is_some() {
                theme.warn
            } else {
                theme.dim
            }),
        ));
        spans.push(Span::raw("  "));

        Rendered {
            lines: vec![Line::from(spans)],
            hits,
        }
    }
}
