//! Mouse hit testing.
//!
//! Drawing builds a rect-to-action table; a click is just a lookup by coordinate.
//! Widgets never handle events themselves.

use ratatui::layout::Rect;

pub use crate::sources::audio::Target as AudioTarget;

/// What a click or a wheel event does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Volume bar. The click position maps to 0..=100 percent.
    ///
    /// Output and input are drawn alike, so the target rides along with the action:
    /// the display would otherwise be symmetrical while the controls were not.
    VolumeBar {
        target: AudioTarget,
        x: u16,
        width: u16,
    },
    /// Toggle mute. The icon is what carries it.
    ToggleMute {
        target: AudioTarget,
    },
    /// A workspace cell. Clicking it switches to that workspace.
    Workspace {
        id: i64,
    },
    /// A name on the network selector. Clicking it shows that interface.
    ///
    /// The index is into `state.network.latest.interfaces`, which is the list the reducer
    /// sees too - a position on the row would make it reproduce the selector's width
    /// arithmetic. What is *stored* afterwards is the name, not this index: the list
    /// is re-sorted every refresh and a container starting would otherwise move the
    /// selection onto a different interface.
    NetworkSelect {
        index: u8,
    },
    /// Seek bar. The click position maps to a 0.0..=1.0 ratio.
    SeekBar {
        x: u16,
        width: u16,
    },
    Previous,
    PlayPause,
    Next,
    /// Power button. Pressing it opens the confirmation modal.
    Power(crate::sources::power::PowerAction),
    ConfirmYes,
    ConfirmNo,
    /// Outside the modal. A click there cancels.
    ConfirmCancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub area: Rect,
    pub action: Action,
}

/// The table, rebuilt on every draw.
#[derive(Debug, Clone, Default)]
pub struct HitMap {
    hits: Vec<Hit>,
    /// The frame the album art goes in, in screen cells. Not a click target, but
    /// likewise something only drawing can say: the event loop hands it to the art
    /// provider.
    pub art: Option<Rect>,
}

impl HitMap {
    pub fn clear(&mut self) {
        self.hits.clear();
        self.art = None;
    }

    pub fn push(&mut self, area: Rect, action: Action) {
        self.hits.push(Hit { area, action });
    }

    /// Return the action for a coordinate. Later registrations win
    /// (so an overlay can cover what is underneath).
    pub fn at(&self, x: u16, y: u16) -> Option<Action> {
        self.hits
            .iter()
            .rev()
            .find(|h| {
                x >= h.area.x
                    && x < h.area.x + h.area.width
                    && y >= h.area.y
                    && y < h.area.y + h.area.height
            })
            .map(|h| h.action)
    }
}

/// Map an x coordinate on the volume bar to 0..=100 percent.
///
/// Even the leftmost column yields one step rather than 0%
/// (clicking and getting silence would be counter-intuitive).
pub fn volume_at(bar_x: u16, bar_width: u16, click_x: u16) -> u16 {
    if bar_width == 0 {
        return 0;
    }
    let offset = click_x.saturating_sub(bar_x).min(bar_width - 1);
    let ratio = (offset as f32 + 1.0) / bar_width as f32;
    (ratio * 100.0).round() as u16
}

/// Map an x coordinate on the seek bar to a 0.0..=1.0 ratio.
pub fn ratio_at(bar_x: u16, bar_width: u16, click_x: u16) -> f64 {
    if bar_width <= 1 {
        return 0.0;
    }
    let offset = click_x.saturating_sub(bar_x).min(bar_width - 1);
    offset as f64 / (bar_width - 1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mute() -> Action {
        Action::ToggleMute {
            target: AudioTarget::Sink,
        }
    }

    fn bar() -> Action {
        Action::VolumeBar {
            target: AudioTarget::Sink,
            x: 0,
            width: 10,
        }
    }

    #[test]
    fn finds_the_action_under_the_cursor() {
        let mut map = HitMap::default();
        map.push(Rect::new(10, 5, 4, 1), mute());
        assert_eq!(map.at(10, 5), Some(mute()));
        assert_eq!(map.at(13, 5), Some(mute()));
        assert_eq!(map.at(14, 5), None);
        assert_eq!(map.at(10, 6), None);
        assert_eq!(map.at(9, 5), None);
    }

    #[test]
    fn later_registrations_win() {
        let mut map = HitMap::default();
        map.push(Rect::new(0, 0, 10, 1), mute());
        map.push(Rect::new(0, 0, 10, 1), bar());
        assert_eq!(map.at(5, 0), Some(bar()));
    }

    #[test]
    fn clear_empties_the_map() {
        let mut map = HitMap::default();
        map.push(Rect::new(0, 0, 1, 1), mute());
        map.clear();
        assert_eq!(map.at(0, 0), None);
    }

    #[test]
    fn volume_spans_the_whole_bar() {
        // Even the leftmost column is not 0%.
        assert_eq!(volume_at(10, 20, 10), 5);
        assert_eq!(volume_at(10, 20, 19), 50);
        assert_eq!(volume_at(10, 20, 29), 100);
    }

    #[test]
    fn clicks_outside_the_bar_are_clamped() {
        assert_eq!(volume_at(10, 20, 0), 5);
        assert_eq!(volume_at(10, 20, 999), 100);
    }

    #[test]
    fn seek_ratio_spans_zero_to_one() {
        assert_eq!(ratio_at(10, 21, 10), 0.0);
        assert!((ratio_at(10, 21, 20) - 0.5).abs() < 1e-9);
        assert_eq!(ratio_at(10, 21, 30), 1.0);
        // Out-of-range input still clamps to 0..=1.
        assert_eq!(ratio_at(10, 21, 0), 0.0);
        assert_eq!(ratio_at(10, 21, 999), 1.0);
    }

    #[test]
    fn zero_width_bar_is_safe() {
        assert_eq!(volume_at(0, 0, 5), 0);
    }
}
