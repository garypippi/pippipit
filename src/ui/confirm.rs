//! The confirmation modal for power actions.
//!
//! **Focus always defaults to No,** so a stray Enter does nothing.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::sources::power::PowerAction;
use crate::ui::hit::{Action, HitMap};
use crate::ui::theme::Theme;

/// Outer size of the modal.
const WIDTH: u16 = 46;
const HEIGHT: u16 = 9;

/// The rect it occupies when centred on screen.
pub fn area_for(screen: Rect) -> Rect {
    let w = WIDTH.min(screen.width);
    let h = HEIGHT.min(screen.height);
    Rect {
        x: screen.x + (screen.width.saturating_sub(w)) / 2,
        y: screen.y + (screen.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

pub fn draw(
    frame: &mut Frame,
    screen: Rect,
    action: PowerAction,
    yes_focused: bool,
    theme: &Theme,
    hits: &mut HitMap,
) {
    let area = area_for(screen);

    // Register the whole screen first, so a click outside the modal counts as cancel.
    hits.push(screen, Action::ConfirmCancel);

    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.crit))
        .title(Span::styled(
            " Confirm ",
            Style::default().fg(theme.crit).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let focus = Style::default()
        .fg(theme.crit)
        .add_modifier(Modifier::BOLD | Modifier::REVERSED);
    let plain = Style::default().fg(theme.text);

    let yes = "[ Yes ]";
    let no = "[ No ]";
    let gap = 8;
    let buttons_width = yes.len() + gap + no.len();
    let buttons_indent = (inner.width as usize).saturating_sub(buttons_width) / 2;

    let lines = vec![
        Line::raw(""),
        Line::styled(
            format!(
                "{}{}",
                " ".repeat((inner.width as usize).saturating_sub(action.question().len()) / 2),
                action.question()
            ),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::from(vec![
            Span::raw(" ".repeat(buttons_indent)),
            Span::styled(yes, if yes_focused { focus } else { plain }),
            Span::raw(" ".repeat(gap)),
            Span::styled(no, if yes_focused { plain } else { focus }),
        ]),
        Line::raw(""),
        Line::styled(
            "      y / Enter: yes      Esc / n: no",
            Style::default().fg(theme.dim),
        ),
    ];

    // Hit areas for the buttons. The row numbers must match the order of `lines` above.
    let buttons_y = inner.y + 3;
    let yes_x = inner.x + buttons_indent as u16;
    hits.push(
        Rect::new(yes_x, buttons_y, yes.len() as u16, 1),
        Action::ConfirmYes,
    );
    hits.push(
        Rect::new(
            yes_x + (yes.len() + gap) as u16,
            buttons_y,
            no.len() as u16,
            1,
        ),
        Action::ConfirmNo,
    );

    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modal_is_centred_and_fits() {
        let screen = Rect::new(0, 0, 145, 18);
        let a = area_for(screen);
        assert_eq!(a.width, WIDTH);
        assert_eq!(a.height, HEIGHT);
        assert_eq!(a.x + a.width / 2, screen.width / 2);
    }

    /// It must not overflow even on a small terminal.
    #[test]
    fn modal_shrinks_to_fit_a_small_screen() {
        let screen = Rect::new(0, 0, 20, 5);
        let a = area_for(screen);
        assert!(a.width <= screen.width);
        assert!(a.height <= screen.height);
    }
}
