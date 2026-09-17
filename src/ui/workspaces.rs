//! The workspace row.
//!
//! One row for every monitor wasted three rows and still ran out of columns, so
//! the monitors are folded into **a single row** keyed by workspace id. The id is
//! unique across monitors, and the id is what the keybind uses, so the monitor
//! name carries little at a glance; which monitor a workspace is active on is
//! left to the cell style instead.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::sources::hyprland::{Tile, Workspaces};
use crate::store::WorkspaceStore;
use crate::ui::hit::Action;
use crate::ui::slot::{DrawCtx, Measure, Rendered, Slot, priority};
use crate::ui::theme::{Part, Theme};

/// What a cell says about its workspace. The first match wins, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellState {
    /// Active on the focused monitor: the one the keyboard is on.
    Focused,
    /// Active on some other monitor - visible, but not where the input goes.
    ActiveElsewhere,
    /// Holds at least one window.
    Occupied,
    /// Exists and is empty.
    Idle,
    /// Not created yet. Hyprland only reports the workspaces that exist, so
    /// `1..=count` is filled in to keep the row from changing width as they come and go.
    Missing,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub id: i64,
    pub state: CellState,
    /// Where the windows sit. The mini-map is drawn from these.
    pub tiles: Vec<Tile>,
}

/// Fold the per-monitor rows into the cells of the single row.
///
/// The ids drawn are `1..=count`, plus anything that exists or is active beyond it
/// (a pinned workspace 10 must not vanish just because `count` is 8).
pub fn cells(workspaces: &Workspaces, count: i64) -> Vec<Cell> {
    // No monitor reporting focus is possible; fall back to the first so that exactly
    // one cell is still marked as the current one.
    let focused = workspaces
        .monitors
        .iter()
        .find(|m| m.focused)
        .or_else(|| workspaces.monitors.first());
    let focused_active = focused.map(|m| m.active);

    let mut ids: Vec<i64> = (1..=count.max(0)).collect();
    for monitor in &workspaces.monitors {
        ids.extend(monitor.workspaces.iter().map(|c| c.id));
        if monitor.active >= 0 {
            ids.push(monitor.active);
        }
    }
    ids.retain(|id| *id >= 0);
    ids.sort_unstable();
    ids.dedup();

    ids.into_iter()
        .map(|id| {
            let cell = workspaces
                .monitors
                .iter()
                .flat_map(|m| m.workspaces.iter())
                .find(|c| c.id == id);
            let occupied = cell.is_some_and(|c| c.occupied);
            let active_elsewhere = workspaces
                .monitors
                .iter()
                .any(|m| m.active == id && Some(m.active) != focused_active);
            let state = if focused_active == Some(id) {
                CellState::Focused
            } else if active_elsewhere {
                CellState::ActiveElsewhere
            } else if occupied {
                CellState::Occupied
            } else if cell.is_some() {
                CellState::Idle
            } else {
                CellState::Missing
            };
            Cell {
                id,
                state,
                tiles: cell.map(|c| c.tiles.clone()).unwrap_or_default(),
            }
        })
        .collect()
}

/// The mini-map grid for one workspace, in pixels.
///
/// Square on both axes, because a terminal cell is about twice as tall as it is wide
/// and two pixel rows share a cell: five by five pixels comes out five columns by two
/// and a half rows, which reads as a square. Odd on both axes so that a split leaves
/// the middle pixel for the gap, which is what caps the map at three windows per axis
/// - nine in a grid.
pub const MAP_COLUMNS: usize = 5;
const MAP_PIXEL_ROWS: usize = 5;
/// Cell rows the map occupies: two pixel rows per cell, rounded up.
pub const MAP_HEIGHT: usize = MAP_PIXEL_ROWS.div_ceil(2);

/// How far a window is shrunk before it is rasterised, as a fraction of the monitor.
///
/// Without it two windows meeting at the middle both claim the pixel on the boundary
/// and the split disappears.
const SHRINK: f32 = 0.05;

/// Rasterise a workspace's windows onto the pixel grid.
///
/// A pixel is filled when its centre falls inside a shrunk window, and remembers
/// **which** window claimed it, so merges can be spotted afterwards.
fn rasterise(tiles: &[Tile]) -> Grid {
    let mut grid: Grid = [[None; MAP_COLUMNS]; MAP_PIXEL_ROWS];
    for (index, tile) in tiles.iter().enumerate() {
        let left = tile.x + SHRINK;
        let right = tile.x + tile.width - SHRINK;
        let top = tile.y + SHRINK;
        let bottom = tile.y + tile.height - SHRINK;
        for (row, cells) in grid.iter_mut().enumerate() {
            let cy = (row as f32 + 0.5) / MAP_PIXEL_ROWS as f32;
            if cy < top || cy > bottom {
                continue;
            }
            for (column, cell) in cells.iter_mut().enumerate() {
                let cx = (column as f32 + 0.5) / MAP_COLUMNS as f32;
                if cx >= left && cx <= right {
                    *cell = Some(index);
                }
            }
        }
    }
    grid
}

/// Which pixel each window claimed, or `None` where nothing did.
type Grid = [[Option<usize>; MAP_COLUMNS]; MAP_PIXEL_ROWS];
type Mask = [[bool; MAP_COLUMNS]; MAP_PIXEL_ROWS];

/// Mark the pixels whose connected run holds more than one window.
///
/// Those are exactly the pixels where the map would otherwise lie: two windows that
/// touch with no gap between them are indistinguishable from one twice the size, and
/// a merged run reads as **one big window** - the worst thing the map could say.
///
/// The marked pixels are drawn in their own colour rather than a texture: a texture
/// in among the solid blocks reads as a rendering fault. Only the run that merged is
/// recoloured, so the windows that did fit still look like themselves, and the rule
/// underneath still carries the workspace state.
fn merged(grid: &Grid) -> Mask {
    let mut mask: Mask = [[false; MAP_COLUMNS]; MAP_PIXEL_ROWS];
    let mut seen: Mask = [[false; MAP_COLUMNS]; MAP_PIXEL_ROWS];
    for row in 0..MAP_PIXEL_ROWS {
        for column in 0..MAP_COLUMNS {
            if grid[row][column].is_none() || seen[row][column] {
                continue;
            }
            // Flood fill, collecting the run and the windows inside it.
            let mut run = vec![(row, column)];
            let mut stack = vec![(row, column)];
            let mut owner = grid[row][column];
            let mut mixed = false;
            seen[row][column] = true;
            while let Some((y, x)) = stack.pop() {
                if grid[y][x] != owner {
                    mixed = true;
                }
                let neighbours = [
                    (y.wrapping_sub(1), x),
                    (y + 1, x),
                    (y, x.wrapping_sub(1)),
                    (y, x + 1),
                ];
                for (ny, nx) in neighbours {
                    if ny < MAP_PIXEL_ROWS
                        && nx < MAP_COLUMNS
                        && grid[ny][nx].is_some()
                        && !seen[ny][nx]
                    {
                        seen[ny][nx] = true;
                        run.push((ny, nx));
                        stack.push((ny, nx));
                    }
                }
            }
            // A window swallowed whole by another leaves no pixel of its own; the run
            // still has to be marked, so compare the run against the windows in it.
            if mixed {
                owner = None;
            }
            if owner.is_none() {
                for (y, x) in run {
                    mask[y][x] = true;
                }
            }
        }
    }
    mask
}

/// Mark where a window too small to claim a pixel of its own would have been.
///
/// Shrinking is what makes the splits visible, but it also swallows a window narrower
/// than about a fifth of the monitor. Such a window is not merged with anything - it
/// is simply missing - so its own area is textured to say that something is there.
fn mark_vanished(tiles: &[Tile], grid: &Grid, mask: &mut Mask) {
    for (index, tile) in tiles.iter().enumerate() {
        if grid.iter().any(|row| row.contains(&Some(index))) {
            continue;
        }
        let mut marked = false;
        for (row, cells) in mask.iter_mut().enumerate() {
            let cy = (row as f32 + 0.5) / MAP_PIXEL_ROWS as f32;
            if cy < tile.y || cy > tile.y + tile.height {
                continue;
            }
            for (column, cell) in cells.iter_mut().enumerate() {
                let cx = (column as f32 + 0.5) / MAP_COLUMNS as f32;
                if cx >= tile.x && cx <= tile.x + tile.width {
                    *cell = true;
                    marked = true;
                }
            }
        }
        if marked {
            continue;
        }
        // Smaller than a single pixel on both axes: put it on the one under its centre.
        let column = ((tile.x + tile.width / 2.0) * MAP_COLUMNS as f32) as usize;
        let row = ((tile.y + tile.height / 2.0) * MAP_PIXEL_ROWS as f32) as usize;
        if row < MAP_PIXEL_ROWS && column < MAP_COLUMNS {
            mask[row][column] = true;
        }
    }
}

/// One row of the map: the glyphs, and which of them hold windows that did not fit.
type MapRow = (String, [bool; MAP_COLUMNS]);

/// Pack the pixel rows into cells, two at a time - the same trick the clock uses.
fn map_rows(tiles: &[Tile]) -> [MapRow; MAP_HEIGHT] {
    let grid = rasterise(tiles);
    let mut mask = merged(&grid);
    mark_vanished(tiles, &grid, &mut mask);

    let mut rows = [const { (String::new(), [false; MAP_COLUMNS]) }; MAP_HEIGHT];
    for (i, (glyphs, crowded)) in rows.iter_mut().enumerate() {
        for column in 0..MAP_COLUMNS {
            let over = mask[i * 2][column];
            let under = mask.get(i * 2 + 1).is_some_and(|r| r[column]);
            // A window that vanished has no pixel of its own, so the mask is what
            // paints it; without this it would be marked but invisible.
            let top = grid[i * 2][column].is_some() || over;
            let bottom = grid.get(i * 2 + 1).is_some_and(|r| r[column].is_some()) || under;
            crowded[column] = over || under;
            glyphs.push(match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
    }
    rows
}

/// Split a map row into runs of glyphs that share a colour.
fn runs(glyphs: &str, crowded: &[bool; MAP_COLUMNS]) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    for (glyph, over) in glyphs.chars().zip(crowded) {
        match out.last_mut() {
            Some((chunk, flag)) if *flag == *over => chunk.push(glyph),
            _ => out.push((glyph.to_string(), *over)),
        }
    }
    out
}

/// The one glyph a slot with no windows draws, in the middle cell of its map.
///
/// With no rule under the slots, this is the only thing an empty workspace puts on
/// screen - and `Missing` holding its position is what keeps
/// "position is the id" true.
fn empty_glyph(state: CellState) -> char {
    match state {
        // The one worth spotting fast, so it gets the heaviest glyph.
        CellState::Focused => '━',
        // `Occupied` with no tiles means Hyprland says there are windows but has not
        // said where; drawing nothing at all would hide the slot entirely.
        CellState::ActiveElsewhere | CellState::Occupied | CellState::Idle => '─',
        // Never created. A dot still holds the position.
        CellState::Missing => '·',
    }
}

/// The rows an empty slot draws: the glyph in the middle cell, blanks around it.
///
/// Never overlaid on a map that has windows in it, so it cannot collide with them.
fn empty_rows(state: CellState) -> [String; MAP_HEIGHT] {
    std::array::from_fn(|row| {
        if row != MAP_HEIGHT / 2 {
            return " ".repeat(MAP_COLUMNS);
        }
        let left = MAP_COLUMNS / 2;
        let mut out = " ".repeat(left);
        out.push(empty_glyph(state));
        out.push_str(&" ".repeat(MAP_COLUMNS - left - 1));
        out
    })
}

/// The colour of a slot. Only the two active workspaces are accented: with the rule
/// gone the strip is all blocks, and every window at full brightness drowns out the
/// one thing being looked for.
fn cell_style(state: CellState, theme: &Theme) -> Style {
    match state {
        CellState::Focused => Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
        // Visible on the other monitor, but not where you are typing.
        CellState::ActiveElsewhere => Style::default().fg(theme.accent),
        CellState::Occupied => Style::default().fg(theme.dim),
        CellState::Idle => Style::default().fg(theme.dim),
        CellState::Missing => Style::default().fg(theme.border),
    }
}

/// Columns one slot occupies, plus the single column of gap that follows it.
const SLOT_STRIDE: u16 = MAP_COLUMNS as u16 + 1;

/// Columns one pill occupies, plus the single column of gap that follows it.
pub const PILL_COLUMNS: u16 = 2;
pub const PILL_STRIDE: u16 = PILL_COLUMNS + 1;

/// How the strip is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strip {
    /// Three rows of mini-map: what is in each workspace, not only that it is used.
    Map,
    /// One row of pills. It fits in the columns beside the clock, where three rows of
    /// map do not; what it gives up for them is the window layout, so a pill says only
    /// whether a workspace has anything in it.
    Pills,
}

/// The pills that fit in `width` columns, and how many slots are left over.
///
/// The last pill needs no gap after it. When the slots outrun the columns, the final
/// pill's place goes to a `+n` saying how many are not drawn - the strip keeps its one
/// row whatever Hyprland is doing, so the blocks below it never move.
fn pills_that_fit(slots: usize, width: u16) -> (usize, usize) {
    let whole = |n: usize| (n as u16) * PILL_STRIDE - 1;
    if slots == 0 || width < PILL_COLUMNS {
        return (0, slots);
    }
    if whole(slots) <= width {
        return (slots, 0);
    }
    // Every pill dropped is one the marker has to account for, and a wider count can
    // cost another pill, so the two are settled together.
    for shown in (0..slots).rev() {
        let hidden = slots - shown;
        let marker = format!("+{hidden}").chars().count() as u16;
        let used = if shown == 0 {
            marker
        } else {
            whole(shown) + 1 + marker
        };
        if used <= width {
            return (shown, hidden);
        }
    }
    (0, slots)
}

/// The glyph pair one pill draws.
///
/// **Upper half blocks, because the strip sits on the clock's last row.** A digit is
/// five pixel rows, so its last row is drawn as upper halves too: a full block there
/// would hang half a cell below the digits, and these end exactly where they do.
/// A workspace with something in it gets the bar, an empty one a dot holding its
/// position, and colour says which of the two active workspaces is the focused one.
fn pill_glyphs(state: CellState) -> &'static str {
    match state {
        CellState::Focused | CellState::ActiveElsewhere | CellState::Occupied => "▀▀",
        // A dot in the left column: two dots would read as two workspaces.
        CellState::Idle | CellState::Missing => "· ",
    }
}

/// What the workspace map reads.
pub struct WorkspacesSlot<'a> {
    pub workspaces: &'a WorkspaceStore,
    /// How many slots the strip shows, whether or not Hyprland reports that many.
    pub count: i64,
    /// Whether a slot is clickable at all.
    pub clickable: bool,
    /// Map or pills.
    pub strip: Strip,
}

impl Slot for WorkspacesSlot<'_> {
    fn part(&self) -> Part {
        Part::Workspaces
    }

    /// Three rows of map, one row of pills, or the one row that says why there is none.
    fn measure(&self, _width: u16) -> Measure {
        let rows = if self.workspaces.error.is_some() && self.workspaces.latest.monitors.is_empty()
        {
            1
        } else {
            match self.strip {
                Strip::Map => MAP_HEIGHT as u16,
                Strip::Pills => 1,
            }
        };
        Measure {
            // A map with a row missing is not a map, so it goes whole or not at all.
            min: rows,
            preferred: rows,
            priority: priority::WORKSPACES,
        }
    }

    /// The strip: `MAP_HEIGHT` rows of mini-map, one row per slot and nothing under them.
    ///
    /// Every slot is the same width and they are drawn in id order, so **position is the
    /// id** and the numbers do not need printing. A slot with no windows draws
    /// `empty_glyph` in its middle cell, which is what keeps the positions countable now
    /// that the rule under the strip is gone.
    ///
    /// `area` is where the strip lands on screen, so each slot can register the rect a
    /// click on it belongs to.
    fn render(&self, ctx: &DrawCtx, area: Rect) -> Rendered<'static> {
        let theme = ctx.theme;
        let mut hits: Vec<(Rect, Action)> = Vec::new();
        if let Some(err) = &self.workspaces.error
            && self.workspaces.latest.monitors.is_empty()
        {
            return Rendered::new(vec![Line::styled(
                format!("hyprland unavailable: {err}"),
                Style::default().fg(theme.dim),
            )]);
        }

        let cells = cells(&self.workspaces.latest, self.count);
        if self.strip == Strip::Pills {
            return self.pills(&cells, theme, area);
        }
        let mut rows: Vec<Vec<Span>> = vec![Vec::new(); MAP_HEIGHT];
        for (i, cell) in cells.iter().enumerate() {
            if i > 0 {
                // Between the slots only: a trailing column would push the last slot out of
                // the 64-column pane a 130-column terminal gives.
                for row in &mut rows {
                    row.push(Span::raw(" "));
                }
            }
            let style = cell_style(cell.state, theme);
            let crowded_style = Style::default().fg(theme.warn);
            if cell.tiles.is_empty() {
                // Keyed off the tiles, not off the rendered map. `mark_vanished` gives
                // every tile a pixel except one entirely off its monitor, and judging by
                // the render would put an empty slot's glyph on a workspace that has a
                // window in it.
                for (row, glyphs) in empty_rows(cell.state).into_iter().enumerate() {
                    rows[row].push(Span::styled(glyphs, style));
                }
            } else {
                for (row, (glyphs, crowded)) in map_rows(&cell.tiles).into_iter().enumerate() {
                    // One span per run of the same colour, so a slot is at most a few spans.
                    for (chunk, over) in runs(&glyphs, &crowded) {
                        rows[row].push(Span::styled(
                            chunk,
                            if over { crowded_style } else { style },
                        ));
                    }
                }
            }

            // The whole slot is the target, empty ones included: the map rows are
            // registered whatever is drawn in them.
            let x = area.x + i as u16 * SLOT_STRIDE;
            if self.clickable && x + MAP_COLUMNS as u16 <= area.x + area.width {
                hits.push((
                    Rect::new(x, area.y, MAP_COLUMNS as u16, MAP_HEIGHT as u16),
                    Action::Workspace { id: cell.id },
                ));
            }
        }
        Rendered {
            lines: rows.into_iter().map(Line::from).collect(),
            hits,
        }
    }
}

impl WorkspacesSlot<'_> {
    /// The one-row strip: a pill per workspace, and a `+n` for the ones that ran out
    /// of columns.
    ///
    /// `area` is where the strip lands on screen, so each pill can register the rect a
    /// click on it belongs to.
    fn pills(&self, cells: &[Cell], theme: &Theme, area: Rect) -> Rendered<'static> {
        let mut hits: Vec<(Rect, Action)> = Vec::new();
        let mut spans: Vec<Span> = Vec::new();
        let (shown, hidden) = pills_that_fit(cells.len(), area.width);

        // Centred in the columns it was given, so the strip reads as one thing placed
        // beside the clock rather than as a run starting at the clock's edge. What is
        // drawn is measured first, the marker included, and the hit areas move with it.
        let drawn = match (shown, hidden) {
            (0, 0) => 0,
            (0, n) => format!("+{n}").chars().count() as u16,
            (s, 0) => s as u16 * PILL_STRIDE - 1,
            (s, n) => s as u16 * PILL_STRIDE + format!("+{n}").chars().count() as u16,
        };
        let pad = area.width.saturating_sub(drawn) / 2;
        if pad > 0 {
            spans.push(Span::raw(" ".repeat(pad as usize)));
        }
        let area = Rect {
            x: area.x + pad,
            ..area
        };

        for (i, cell) in cells.iter().take(shown).enumerate() {
            if i > 0 {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(
                pill_glyphs(cell.state),
                cell_style(cell.state, theme),
            ));
            // The gap belongs to the pill on its left: two columns is a small target,
            // and the areas still cannot overlap.
            let columns = if i + 1 == shown {
                PILL_COLUMNS
            } else {
                PILL_STRIDE
            };
            if self.clickable {
                hits.push((
                    Rect::new(area.x + i as u16 * PILL_STRIDE, area.y, columns, 1),
                    Action::Workspace { id: cell.id },
                ));
            }
        }

        if hidden > 0 {
            if shown > 0 {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(
                format!("+{hidden}"),
                Style::default().fg(theme.dim),
            ));
        }

        Rendered {
            lines: vec![Line::from(spans)],
            hits,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::hyprland::{MonitorRow, WorkspaceCell};
    use crate::ui::hit::HitMap;

    fn monitor(name: &str, focused: bool, active: i64, ids: &[(i64, bool)]) -> MonitorRow {
        MonitorRow {
            name: name.into(),
            active,
            focused,
            workspaces: ids
                .iter()
                .map(|(id, occupied)| WorkspaceCell {
                    id: *id,
                    occupied: *occupied,
                    tiles: Vec::new(),
                })
                .collect(),
        }
    }

    fn sample() -> Workspaces {
        Workspaces {
            monitors: vec![
                monitor("DP-1", true, 2, &[(1, true), (2, false)]),
                monitor("DP-2", false, 10, &[(10, true)]),
            ],
        }
    }

    /// Draw the strip somewhere with room to spare and keep the hit areas.
    fn draw(store: &WorkspaceStore) -> (Vec<Line<'static>>, HitMap) {
        draw_into(store, Rect::new(0, 0, 80, MAP_HEIGHT as u16))
    }

    fn draw_into(store: &WorkspaceStore, area: Rect) -> (Vec<Line<'static>>, HitMap) {
        let out = WorkspacesSlot {
            workspaces: store,
            count: crate::config::WorkspacesConfig::default().count,
            clickable: true,
            strip: Strip::Map,
        }
        .render(
            &DrawCtx {
                theme: &Theme::default(),
            },
            area,
        );
        let mut hits = HitMap::default();
        for (rect, action) in out.hits {
            hits.push(rect, action);
        }
        (out.lines, hits)
    }

    /// The strip in pill form, in `columns` columns: what it drew, and where a click on
    /// it lands.
    fn pills_into(store: &WorkspaceStore, columns: u16) -> (String, HitMap) {
        let out = WorkspacesSlot {
            workspaces: store,
            count: crate::config::WorkspacesConfig::default().count,
            clickable: true,
            strip: Strip::Pills,
        }
        .render(
            &DrawCtx {
                theme: &Theme::default(),
            },
            Rect::new(0, 0, columns, 1),
        );
        let mut hits = HitMap::default();
        for (rect, action) in out.hits {
            hits.push(rect, action);
        }
        let text = out.lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        (text, hits)
    }

    /// The pill strip is one row whatever the map would have taken: a bar for a
    /// workspace with something in it, a dot for an empty one, and colour for which of
    /// the two active ones has the focus. It is centred in the columns it was given, and
    /// the hit areas move with it.
    #[test]
    fn pills_say_the_state_in_one_row() {
        let (text, hits) = pills_into(&sample_store(), 40);
        let pad = " ".repeat(7); // (40 - 26) / 2
        assert_eq!(text, format!("{pad}▀▀ ▀▀ ·  ·  ·  ·  ·  ·  ▀▀"));
        assert_eq!(hits.at(0, 0), None, "the padding takes no clicks");
        assert_eq!(hits.at(7, 0), Some(Action::Workspace { id: 1 }));
        assert_eq!(hits.at(10, 0), Some(Action::Workspace { id: 2 }));
        // Position is the id, so the workspace past `count` is the one on the end.
        assert_eq!(hits.at(31, 0), Some(Action::Workspace { id: 10 }));
    }

    /// Past the columns on offer the strip says how many pills it is not drawing. It
    /// never grows a row or runs into whatever is beside it.
    #[test]
    fn pills_that_do_not_fit_become_a_count() {
        assert_eq!(pills_that_fit(10, 29), (10, 0), "ten pills is 29 columns");
        assert_eq!(
            pills_that_fit(10, 28),
            (8, 2),
            "a marker one column short of the gap costs a whole pill"
        );
        assert_eq!(pills_that_fit(0, 40), (0, 0));

        let (text, hits) = pills_into(&sample_store(), 12);
        assert_eq!(text, "▀▀ ▀▀ ·  +6");
        assert_eq!(hits.at(9, 0), None, "the marker takes no clicks");
    }

    fn sample_store() -> WorkspaceStore {
        WorkspaceStore {
            latest: sample(),
            ..WorkspaceStore::default()
        }
    }

    fn state_of(id: i64, cells: &[Cell]) -> CellState {
        cells.iter().find(|c| c.id == id).expect("cell").state
    }

    fn tile(x: f32, y: f32, width: f32, height: f32) -> Tile {
        Tile {
            x,
            y,
            width,
            height,
        }
    }

    fn map(tiles: &[Tile]) -> Vec<String> {
        map_rows(tiles)
            .into_iter()
            .map(|(glyphs, _)| glyphs)
            .collect()
    }

    /// The columns of each row that hold windows the grid could not fit.
    fn crowded(tiles: &[Tile]) -> Vec<[bool; MAP_COLUMNS]> {
        map_rows(tiles).into_iter().map(|(_, over)| over).collect()
    }

    #[test]
    fn fills_up_to_count_and_keeps_the_ids_beyond_it() {
        let cells = cells(&sample(), 8);
        assert_eq!(
            cells.iter().map(|c| c.id).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5, 6, 7, 8, 10]
        );
    }

    #[test]
    fn distinguishes_focus_from_the_other_monitor() {
        let cells = cells(&sample(), 8);
        assert_eq!(state_of(2, &cells), CellState::Focused);
        assert_eq!(state_of(10, &cells), CellState::ActiveElsewhere);
        assert_eq!(state_of(1, &cells), CellState::Occupied);
        assert_eq!(state_of(3, &cells), CellState::Missing);
    }

    /// An existing but empty workspace is not the same as one that was never created.
    #[test]
    fn idle_and_missing_are_separate() {
        let ws = Workspaces {
            monitors: vec![monitor("DP-1", true, 1, &[(1, true), (4, false)])],
        };
        let cells = cells(&ws, 8);
        assert_eq!(state_of(4, &cells), CellState::Idle);
        assert_eq!(state_of(5, &cells), CellState::Missing);
    }

    /// With no monitor claiming focus, one cell is still the current one.
    #[test]
    fn falls_back_to_the_first_monitor_when_none_is_focused() {
        let ws = Workspaces {
            monitors: vec![
                monitor("DP-1", false, 3, &[(3, true)]),
                monitor("DP-2", false, 10, &[(10, true)]),
            ],
        };
        let cells = cells(&ws, 8);
        assert_eq!(state_of(3, &cells), CellState::Focused);
        assert_eq!(state_of(10, &cells), CellState::ActiveElsewhere);
    }

    /// `rows` x `columns` windows, tiled edge to edge.
    fn grid_of(columns: usize, rows: usize) -> Vec<Tile> {
        let (w, h) = (1.0 / columns as f32, 1.0 / rows as f32);
        (0..columns * rows)
            .map(|i| tile((i % columns) as f32 * w, (i / columns) as f32 * h, w, h))
            .collect()
    }

    #[test]
    fn an_empty_workspace_draws_nothing() {
        assert_eq!(map(&[]), ["     ", "     ", "     "]);
    }

    #[test]
    fn one_window_fills_the_slot() {
        assert_eq!(
            map(&[tile(0.0, 0.0, 1.0, 1.0)]),
            ["█████", "█████", "▀▀▀▀▀"]
        );
    }

    /// The gap down the middle is the whole point: without it a split is invisible.
    #[test]
    fn a_side_by_side_split_keeps_the_middle_column_clear() {
        assert_eq!(map(&grid_of(2, 1)), ["██ ██", "██ ██", "▀▀ ▀▀"]);
    }

    /// Four pixel rows would let a top and bottom pair fill every row and read as one
    /// window; five leaves the middle row for the gap.
    #[test]
    fn a_stacked_split_is_not_mistaken_for_a_single_window() {
        let stacked = map(&grid_of(1, 2));
        assert_ne!(stacked, map(&[tile(0.0, 0.0, 1.0, 1.0)]));
        assert_eq!(stacked, ["█████", "▄▄▄▄▄", "▀▀▀▀▀"]);
    }

    /// Dwindle's usual shape: one window down the left, two stacked on the right.
    #[test]
    fn three_windows_read_as_one_plus_a_stack() {
        let tiles = [
            tile(0.0, 0.0, 0.5, 1.0),
            tile(0.5, 0.0, 0.5, 0.5),
            tile(0.5, 0.5, 0.5, 0.5),
        ];
        assert_eq!(map(&tiles), ["██ ██", "██ ▄▄", "▀▀ ▀▀"]);
    }

    #[test]
    fn four_windows_read_as_a_grid() {
        assert_eq!(map(&grid_of(2, 2)), ["██ ██", "▄▄ ▄▄", "▀▀ ▀▀"]);
    }

    /// Three per axis is what the five pixel grid can hold: nine windows still show
    /// nine separate rectangles.
    #[test]
    fn nine_windows_still_read_as_nine() {
        assert_eq!(map(&grid_of(3, 3)), ["▀ ▀ ▀", "▀ ▀ ▀", "▀ ▀ ▀"]);
    }

    /// Past that the neighbours merge, and a merged map reads as one big window -
    /// the opposite of the truth. Those windows get their own colour instead.
    #[test]
    fn windows_that_merge_are_marked_crowded() {
        for row in crowded(&grid_of(4, 3)) {
            assert!(row.iter().any(|c| *c), "{row:?}");
        }
    }

    /// A layout the grid can hold must never be marked.
    #[test]
    fn representable_layouts_are_never_crowded() {
        for tiles in [
            grid_of(1, 1),
            grid_of(2, 1),
            grid_of(1, 2),
            grid_of(2, 2),
            grid_of(3, 1),
            grid_of(3, 3),
        ] {
            for row in crowded(&tiles) {
                assert!(
                    row.iter().all(|c| !*c),
                    "{row:?} for {} windows",
                    tiles.len()
                );
            }
        }
    }

    /// The windows that did fit stay themselves; only the run that merged is recoloured.
    /// This is the real shape of a five-window dwindle: a full-height window on the
    /// right, one across the top left, and three small ones that cannot be separated.
    #[test]
    fn only_the_merged_run_is_marked() {
        let tiles = [
            tile(0.502, 0.035, 0.489, 0.951),
            tile(0.008, 0.035, 0.489, 0.472),
            tile(0.008, 0.515, 0.243, 0.471),
            tile(0.256, 0.752, 0.241, 0.233),
            tile(0.256, 0.515, 0.241, 0.229),
        ];
        // The shapes stay solid: only the colour changes.
        assert_eq!(map(&tiles), ["██ ██", "▄▄ ██", "▀▀ ▀▀"]);
        let over = crowded(&tiles);
        assert_eq!(
            over[0], [false; MAP_COLUMNS],
            "the honest windows keep their colour"
        );
        assert!(
            over[1][1] && over[2][1],
            "the three that could not fit are marked"
        );
        assert!(
            !over[1][0] && !over[1][3],
            "and the marking must not spread"
        );
    }

    #[test]
    fn a_row_becomes_one_span_per_colour_run() {
        let row = runs("██ ██", &[false, true, true, false, false]);
        assert_eq!(
            row,
            vec![
                ("█".to_string(), false),
                ("█ ".to_string(), true),
                ("██".to_string(), false),
            ]
        );
    }

    /// A window off the edge of its monitor must not spill into the next slot.
    #[test]
    fn out_of_range_tiles_do_not_overflow_the_grid() {
        let tiles = [tile(-2.0, -2.0, 8.0, 8.0)];
        for row in map(&tiles) {
            assert_eq!(row.chars().count(), MAP_COLUMNS);
        }
    }

    /// Every slot is the same width, since position is what says which workspace it
    /// is - the slots carry no numbers.
    #[test]
    fn every_slot_is_the_same_width_on_every_row() {
        let rows = draw(&sample_store()).0;
        assert_eq!(
            rows.len(),
            MAP_HEIGHT,
            "the rule row under the strip is gone"
        );
        let width = rows[0].width();
        for row in &rows {
            assert_eq!(row.width(), width, "rows must line up");
        }
        // Nine slots and the eight single-column gaps between them, and no more:
        // a trailing gap would push the last slot out of a 130-column terminal.
        assert_eq!(width, 9 * MAP_COLUMNS + 8);
    }

    const EVERY_STATE: [CellState; 5] = [
        CellState::Focused,
        CellState::ActiveElsewhere,
        CellState::Occupied,
        CellState::Idle,
        CellState::Missing,
    ];

    /// An empty slot has to stay a slot: the same width as one full of windows, or
    /// the positions stop lining up.
    #[test]
    fn an_empty_slot_is_the_same_shape_as_a_full_one() {
        for state in EVERY_STATE {
            let rows = empty_rows(state);
            assert_eq!(rows.len(), MAP_HEIGHT, "{state:?}");
            for row in &rows {
                assert_eq!(row.chars().count(), MAP_COLUMNS, "{state:?} {row:?}");
            }
        }
    }

    /// The glyph sits in the middle cell, and only there.
    #[test]
    fn the_empty_glyph_sits_in_the_middle() {
        let rows = empty_rows(CellState::Idle);
        assert_eq!(rows[MAP_HEIGHT / 2], "  ─  ");
        for (i, row) in rows.iter().enumerate() {
            if i != MAP_HEIGHT / 2 {
                assert_eq!(row, "     ", "row {i} must be blank");
            }
        }
    }

    /// The focused workspace has to be findable when it is empty, and a workspace that
    /// was never created has to hold its position - that is what makes position the id.
    #[test]
    fn an_empty_slot_separates_focus_existing_and_never_created() {
        assert_ne!(
            empty_glyph(CellState::Focused),
            empty_glyph(CellState::Idle)
        );
        assert_ne!(
            empty_glyph(CellState::Idle),
            empty_glyph(CellState::Missing)
        );
        for state in EVERY_STATE {
            assert!(
                !empty_glyph(state).is_whitespace(),
                "{state:?} must be visible"
            );
        }
    }

    /// `dim` and `border` are the same colour by default, so `Idle` and `Missing` are
    /// told apart by their glyph alone. If that stops being true they merge on screen.
    #[test]
    fn idle_and_missing_do_not_rely_on_the_colour() {
        let theme = Theme::default();
        assert_eq!(theme.dim, theme.border, "the premise of this test");
        assert_ne!(
            empty_glyph(CellState::Idle),
            empty_glyph(CellState::Missing)
        );
    }

    /// Only the active workspaces are accented; an ordinary window is dim so it does
    /// not compete with them.
    #[test]
    fn only_the_active_workspaces_are_accented() {
        let theme = Theme::default();
        for state in [CellState::Focused, CellState::ActiveElsewhere] {
            assert_eq!(
                cell_style(state, &theme).fg,
                Some(theme.accent),
                "{state:?}"
            );
        }
        for state in [CellState::Occupied, CellState::Idle] {
            assert_eq!(cell_style(state, &theme).fg, Some(theme.dim), "{state:?}");
        }
    }

    /// Hyprland can call a workspace occupied without having said where its windows
    /// are. Drawing the map from the tiles alone would leave that slot blank.
    #[test]
    fn an_occupied_workspace_with_no_tiles_still_shows_something() {
        let store = sample_store();
        let cells = cells(
            &store.latest,
            crate::config::WorkspacesConfig::default().count,
        );
        let cell = cells.iter().find(|c| c.id == 1).expect("cell 1");
        assert_eq!(cell.state, CellState::Occupied);
        assert!(cell.tiles.is_empty(), "the sample has no tiles");
        let drawn: String = draw(&store).0[MAP_HEIGHT / 2].to_string();
        assert!(
            drawn.starts_with("  ─  "),
            "slot 1 must not be blank: {drawn:?}"
        );
    }

    #[test]
    fn a_hyprland_error_replaces_the_strip() {
        let store = WorkspaceStore {
            error: Some("no socket".into()),
            ..WorkspaceStore::default()
        };
        let text: String = draw(&store).0.iter().map(|l| l.to_string()).collect();
        assert!(text.contains("hyprland unavailable"), "{text}");
    }

    /// The numbers are gone, so a click is only right if position really is the id.
    #[test]
    fn every_slot_is_clickable_at_the_columns_it_is_drawn_in() {
        let store = sample_store();
        let (_, hits) = draw_into(&store, Rect::new(4, 2, 76, MAP_HEIGHT as u16));
        for (i, cell) in cells(
            &store.latest,
            crate::config::WorkspacesConfig::default().count,
        )
        .iter()
        .enumerate()
        {
            let x = 4 + i as u16 * SLOT_STRIDE;
            for column in x..x + MAP_COLUMNS as u16 {
                assert_eq!(
                    hits.at(column, 2),
                    Some(Action::Workspace { id: cell.id }),
                    "slot {i} column {column}"
                );
            }
            // The gap between slots belongs to neither.
            if i > 0 {
                assert_eq!(hits.at(x - 1, 2), None, "the gap before slot {i}");
            }
        }
    }

    /// An empty workspace draws one glyph and blanks, so the target has to be the whole
    /// slot - otherwise the only workspaces you cannot click are the empty ones.
    #[test]
    fn every_row_of_the_slot_is_clickable_even_where_nothing_is_drawn() {
        let (_, hits) = draw(&sample_store());
        for row in 0..MAP_HEIGHT as u16 {
            assert_eq!(
                hits.at(0, row),
                Some(Action::Workspace { id: 1 }),
                "row {row}"
            );
        }
        assert_eq!(hits.at(0, MAP_HEIGHT as u16), None, "below the strip");
    }

    #[test]
    fn click_to_switch_can_be_turned_off() {
        let store = sample_store();
        let out = WorkspacesSlot {
            workspaces: &store,
            count: crate::config::WorkspacesConfig::default().count,
            clickable: false,
            strip: Strip::Map,
        }
        .render(
            &DrawCtx {
                theme: &Theme::default(),
            },
            Rect::new(0, 0, 80, MAP_HEIGHT as u16),
        );
        assert!(out.hits.is_empty());
    }

    /// A pane too narrow to draw the last slots must not leave them clickable where
    /// nothing is drawn.
    #[test]
    fn slots_past_the_edge_are_not_clickable() {
        let store = sample_store();
        let (_, hits) = draw_into(&store, Rect::new(0, 0, 11, MAP_HEIGHT as u16));
        assert_eq!(hits.at(6, 0), Some(Action::Workspace { id: 2 }));
        assert_eq!(hits.at(12, 0), None);
    }

    /// The error branch draws one line and no slots; a stale hit area there would
    /// switch to whatever id happened to be under the cursor.
    #[test]
    fn an_unavailable_hyprland_registers_nothing() {
        let store = WorkspaceStore {
            error: Some("no socket".into()),
            ..WorkspaceStore::default()
        };
        let (_, hits) = draw(&store);
        assert_eq!(hits.at(0, 0), None);
    }

    #[test]
    fn zero_count_is_safe() {
        assert!(cells(&Workspaces::default(), 0).is_empty());
    }
}
