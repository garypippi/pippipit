//! What a pane is made of.
//!
//! A slot knows how many rows it wants and how to draw itself in the rows it gets. It
//! reads one view - the part of the state it draws, and nothing else - and it returns
//! its hit areas rather than writing into a shared table.

use ratatui::layout::Rect;
use ratatui::text::Line;

use super::hit::Action;
use super::theme::{Part, Theme};

/// How many rows a slot wants, the fewest it can do anything with, and how hard it
/// holds on to them.
///
/// A pane that cannot meet `min` leaves the slot out rather than drawing part of it.
pub struct Measure {
    /// Below this there is no point drawing it at all.
    pub min: u16,
    /// What it wants when the pane has room.
    pub preferred: u16,
    /// Higher keeps its rows longer. See [`priority`].
    pub priority: u8,
}

impl Measure {
    /// A slot that is always exactly this tall.
    pub fn fixed(rows: u16, priority: u8) -> Self {
        Self {
            min: rows,
            preferred: rows,
            priority,
        }
    }
}

/// What a slot drew.
#[derive(Default)]
pub struct Rendered<'a> {
    pub lines: Vec<Line<'a>>,
    /// Where a click lands, in screen coordinates.
    pub hits: Vec<(Rect, Action)>,
}

impl<'a> Rendered<'a> {
    pub fn new(lines: Vec<Line<'a>>) -> Self {
        Self {
            lines,
            hits: Vec::new(),
        }
    }
}

/// What every slot is given at draw time: how it looks, not what it says.
pub struct DrawCtx<'a> {
    pub theme: &'a Theme,
}

pub trait Slot {
    /// Whose colours it draws in. The pane hands `render` the theme for this block.
    fn part(&self) -> Part;

    /// The rows this slot wants at `width`. No side effects.
    fn measure(&self, width: u16) -> Measure;

    /// Draw into the rows the pane actually gave it.
    fn render(&self, ctx: &DrawCtx, area: Rect) -> Rendered<'static>;
}

/// What a layout gives up first when it runs out of room.
///
/// The clock is the last thing to go: a panel that cannot say the time has stopped
/// being a panel. The power row is the first, because it is the one block whose job
/// the keyboard cannot do at all - it is mouse-only, and a terminal this small is
/// being read, not clicked.
pub mod priority {
    pub const CLOCK: u8 = 70;
    pub const WORKSPACES: u8 = 60;
    pub const AUDIO: u8 = 50;
    pub const SENSORS: u8 = 40;
    pub const NETWORK: u8 = 30;
    pub const MEDIA: u8 = 20;
    pub const POWER: u8 = 10;
}

/// Hand out `budget` between blocks that want more than there is.
///
/// Rows, for a pane. The bar measures the same way in columns, since what it has to
/// decide is the same: which blocks it can afford to keep.
///
/// Everything gets what it asks for while there is room. Past that, the lowest
/// priority gives up rows first: down to its `min`, and then out of the layout
/// altogether. A slot that is left out gets zero rows, which is the pane's signal not
/// to draw it - a half-drawn block is worse than an absent one, and it would still
/// register clicks for the rows nobody can see.
pub fn allocate(measures: &[Measure], budget: u16) -> Vec<u16> {
    let mut rows: Vec<u16> = measures.iter().map(|m| m.preferred).collect();
    let total = |rows: &[u16]| -> u16 { rows.iter().copied().sum() };

    // Weakest first, and among equals the one further down the pane.
    let mut order: Vec<usize> = (0..measures.len()).collect();
    order.sort_by_key(|i| (measures[*i].priority, std::cmp::Reverse(*i)));

    // Squeeze first: everything that can shrink does, before anything is dropped.
    for &i in &order {
        if total(&rows) <= budget {
            return rows;
        }
        let over = total(&rows) - budget;
        let spare = rows[i].saturating_sub(measures[i].min);
        rows[i] -= spare.min(over);
    }

    // Still over, so some have to go. Filling from the strongest down means a block
    // is left out only when it genuinely does not fit, rather than because something
    // below it was cut first and left a gap nothing can use.
    let mut left = budget;
    let mut granted = vec![0u16; rows.len()];
    for &i in order.iter().rev() {
        if rows[i] <= left {
            granted[i] = rows[i];
            left -= rows[i];
        }
    }
    granted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measure(min: u16, preferred: u16, priority: u8) -> Measure {
        Measure {
            min,
            preferred,
            priority,
        }
    }

    /// Room for everything means nobody has to give anything up.
    #[test]
    fn a_pane_with_room_hands_out_what_was_asked_for() {
        let want = [
            measure(1, 1, priority::CLOCK),
            measure(3, 3, priority::WORKSPACES),
            measure(2, 2, priority::AUDIO),
        ];
        assert_eq!(allocate(&want, 18), vec![1, 3, 2]);
    }

    /// The weakest shrinks to its minimum before anything stronger gives up a row.
    #[test]
    fn the_lowest_priority_shrinks_first() {
        let want = [
            measure(1, 1, priority::CLOCK),
            measure(1, 4, priority::SENSORS),
            measure(1, 5, priority::NETWORK),
        ];
        assert_eq!(allocate(&want, 8), vec![1, 4, 3], "network gives up two");
        assert_eq!(
            allocate(&want, 6),
            vec![1, 4, 1],
            "network down to its minimum"
        );
        assert_eq!(
            allocate(&want, 5),
            vec![1, 3, 1],
            "only then does the panel"
        );
    }

    /// Past shrinking, the weakest leaves altogether rather than everything being
    /// squeezed into rows too few to read.
    #[test]
    fn what_cannot_fit_is_dropped_weakest_first() {
        let want = [
            measure(1, 1, priority::CLOCK),
            measure(3, 3, priority::WORKSPACES),
            measure(2, 2, priority::AUDIO),
            measure(2, 2, priority::MEDIA),
            measure(1, 1, priority::POWER),
        ];
        assert_eq!(allocate(&want, 9), vec![1, 3, 2, 2, 1], "all of it fits");
        assert_eq!(
            allocate(&want, 8),
            vec![1, 3, 2, 2, 0],
            "the power row goes"
        );
        assert_eq!(allocate(&want, 6), vec![1, 3, 2, 0, 0], "then the track");
        assert_eq!(
            allocate(&want, 3),
            vec![1, 0, 2, 0, 0],
            "the map is whole or gone"
        );
        assert_eq!(
            allocate(&want, 1),
            vec![1, 0, 0, 0, 0],
            "the clock is last to go"
        );
    }

    /// Equal priorities are cut from the bottom up, the way a pane is read.
    #[test]
    fn equal_priorities_are_cut_from_the_bottom() {
        let want = [
            measure(0, 2, priority::MEDIA),
            measure(0, 2, priority::MEDIA),
        ];
        assert_eq!(allocate(&want, 2), vec![2, 0]);
    }
}
