//! The framed layouts: two panes side by side, and the single stacked pane a narrower
//! terminal falls back to.
//!
//! A pane is a run of slots down the page with a row budget. The two-pane layout has
//! room for all of them and lays them out in order; the single pane does not, and asks
//! `slot::allocate` which blocks to keep.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::config::{ClockStyle, FrameStyle};
use crate::state::AppState;

use super::draw::{separator, set, write_str};
use super::hit::{Action, HitMap};
use super::slot::{self, DrawCtx, Measure, Slot};
use super::theme::{Part, Theme};
use super::{Mode, audio, clock, media, network, power, sensors, workspaces};

/// The margin every pane keeps on the left, shared by the clock, the map, the mute
/// buttons, the track title and the power row.
const INDENT: u16 = 2;

/// Lays slots out down a pane, one after another.
///
/// Each slot is given the rows its measure asks for, or what is left if that is less.
/// Which slot loses those rows is the pane's decision, not the slot's.
struct Flow<'a> {
    ctx: DrawCtx<'a>,
    area: Rect,
    lines: Vec<Line<'static>>,
    hits: Vec<(Rect, Action)>,
}

impl<'a> Flow<'a> {
    fn new(theme: &'a Theme, area: Rect) -> Self {
        Self {
            ctx: DrawCtx { theme },
            area,
            lines: Vec::new(),
            hits: Vec::new(),
        }
    }

    /// The rows the pane has not handed out yet.
    fn left(&self) -> u16 {
        self.area.height.saturating_sub(self.lines.len() as u16)
    }

    /// The rect the next slot would be drawn into, `indent` columns in.
    fn next_area(&self, rows: u16, indent: u16) -> Rect {
        Rect::new(
            self.area.x + indent,
            self.area.y + self.lines.len() as u16,
            self.area.width.saturating_sub(indent),
            rows,
        )
    }

    fn place(&mut self, slot: &dyn Slot) {
        self.place_indented(slot, 0);
    }

    /// Place a slot in the rows the pane has already decided to give it. Zero rows
    /// means the pane left it out, and nothing is drawn or registered.
    fn place_in(&mut self, slot: &dyn Slot, rows: u16, indent: u16) {
        if rows == 0 {
            return;
        }
        let rendered = self.render(slot, self.next_area(rows, indent));
        self.hits.extend(rendered.hits);
        for line in rendered.lines.into_iter().take(rows as usize) {
            if indent == 0 {
                self.lines.push(line);
            } else {
                let mut spans = vec![Span::raw(" ".repeat(indent as usize))];
                spans.extend(line.spans);
                self.lines.push(Line::from(spans));
            }
        }
    }

    /// As `place`, with every row shifted right - the workspace map sits under the
    /// clock's digits rather than against the frame.
    ///
    /// A slot the pane cannot give `min` rows to is left out entirely, rather than
    /// drawn into rows that are not there: a half-drawn block would still register the
    /// clicks for the parts of it nobody can see.
    fn place_indented(&mut self, slot: &dyn Slot, indent: u16) {
        let measure = slot.measure(self.area.width);
        if self.left() < measure.min {
            return;
        }
        let rows = measure.preferred.min(self.left());
        let rendered = self.render(slot, self.next_area(rows, indent));
        self.hits.extend(rendered.hits);
        for line in rendered.lines {
            if indent == 0 {
                self.lines.push(line);
            } else {
                let mut spans = vec![Span::raw(" ".repeat(indent as usize))];
                spans.extend(line.spans);
                self.lines.push(Line::from(spans));
            }
        }
    }

    /// Draw a slot in its own block's colours.
    fn render(&self, slot: &dyn Slot, area: Rect) -> slot::Rendered<'static> {
        let theme = self.ctx.theme.part(slot.part());
        slot.render(&DrawCtx { theme: &theme }, area)
    }

    fn blank(&mut self) {
        self.lines.push(Line::raw(""));
    }

    fn push(&mut self, line: Line<'static>) {
        self.lines.push(line);
    }

    /// Pad with blanks up to the pane's divider row before stroking it.
    ///
    /// `row` is a cap as much as a position: whatever the pane put above it is
    /// truncated, so lowering it silently drops the bottom of the block above.
    fn pad_to_separator(&mut self, row: usize) {
        while self.lines.len() < row {
            self.lines.push(Line::raw(""));
        }
        self.lines.truncate(row);
        let rule = separator(self.area.width, self.ctx.theme);
        self.lines.push(rule);
        self.lines.push(Line::raw(""));
    }

    fn finish(self, frame: &mut Frame, hits: &mut HitMap) {
        for (rect, action) in self.hits {
            hits.push(rect, action);
        }
        frame.render_widget(Paragraph::new(self.lines), self.area);
    }
}

/// Draw the two panes as **a single frame**.
///
/// Two `Block`s side by side would give two vertical rules, so one outer frame is drawn
/// and the dividers are stroked by hand: one vertical and one horizontal, meeting in a cross.
pub fn draw_two_pane(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    mode: Mode,
    hits: &mut HitMap,
) {
    if area.width < 8 || area.height < 6 {
        super::draw_placeholder(frame, area, state, theme, "tiny");
        return;
    }
    let border = Style::default().fg(theme.border);

    // The sides and the bottom, in columns and rows. `Top` gives them all up; the top
    // rule is drawn either way, since the date is written on it.
    let pad = theme.frame.pad();
    let inner_x = area.x + pad;
    let inner_right = area.right() - pad;
    let inner_w = inner_right - inner_x;

    frame.render_widget(
        Block::default()
            .borders(match theme.frame {
                FrameStyle::Full => Borders::ALL,
                FrameStyle::Top => Borders::TOP,
            })
            .border_style(border),
        area,
    );

    // At 145 columns and a full frame this splits 72 / 70.
    let left_w = (inner_w - 1) / 2 + 1;
    let divider_x = inner_x + left_w;
    let sep_y = area.bottom() - 2 - pad; // the ├──┴──┤ row
    let power_y = sep_y + 1;

    {
        let buf = frame.buffer_mut();

        // The vertical divider.
        for y in (area.y + 1)..sep_y {
            set(buf, divider_x, y, "│", border);
        }
        set(buf, divider_x, area.y, "┬", border);

        // The horizontal rule above the power row. Its ends are the tees that join the
        // sides, or plain rule where there are no sides to join.
        for x in inner_x..inner_right {
            set(buf, x, sep_y, "─", border);
        }
        if pad > 0 {
            set(buf, area.x, sep_y, "├", border);
            set(buf, area.right() - 1, sep_y, "┤", border);
        }
        set(buf, divider_x, sep_y, "┴", border);

        write_date(buf, area, state, theme);
    }

    let content_h = sep_y - area.y - 1;
    let left = Rect::new(inner_x, area.y + 1, left_w, content_h);
    let right = Rect::new(
        divider_x + 1,
        area.y + 1,
        inner_right - (divider_x + 1),
        content_h,
    );
    let power = Rect::new(inner_x, power_y, inner_w, 1);

    draw_left(frame, left, state, theme, mode, hits);
    draw_right(frame, right, state, theme, hits);
    let mut power_flow = Flow::new(theme, power);
    power_flow.place(&power::PowerSlot {
        ui: &state.ui,
        switch_error: state.workspaces.switch_error.as_deref(),
    });
    power_flow.finish(frame, hits);
}

/// The date on the top border.
///
/// The border carries no headings: a program name or a `System` label in accent +
/// bold would say nothing and still be the first thing the eye lands on, where
/// `sensors.rs` keeps its own headings in `theme.dim`.
///
/// The date starts on the clock's left edge one row below - the frame's left column,
/// where there is one, plus the pane's indent - with the space before it one column
/// further left. A column further left would leave the date off the digits it
/// belongs to.
fn write_date(buf: &mut ratatui::buffer::Buffer, area: Rect, state: &AppState, theme: &Theme) {
    let (date, weekday) = date_parts(state);
    let mut x = area.x + theme.frame.pad() + 1;
    // The date is the clock's, though it sits on the frame.
    let theme = theme.part(Part::Clock);
    let text = Style::default().fg(theme.text);
    let dim = Style::default().fg(theme.dim);
    write_str(buf, x, area.y, " ", text);
    x += 1;
    write_str(buf, x, area.y, &date, text);
    x += crate::util::text::width(&date) as u16;
    if !weekday.is_empty() {
        write_str(buf, x, area.y, " ", dim);
        x += 1;
        write_str(buf, x, area.y, &weekday, dim);
        x += crate::util::text::width(&weekday) as u16;
    }
    write_str(buf, x, area.y, " ", text);
}

/// The date, split into the reading and the weekday that restates it.
///
/// Two `strftime` strings rather than one, because a single formatted string gives no
/// way back to which characters came from `%a`, and the two are drawn in different
/// styles. An empty `weekday_format` leaves the weekday out.
fn date_parts(state: &AppState) -> (String, String) {
    (
        state
            .now
            .format(&state.config.clock.date_format)
            .to_string(),
        state
            .now
            .format(&state.config.clock.weekday_format)
            .to_string(),
    )
}

/// The row the divider sits on in the left pane (0-based within the pane).
///
/// It doubles as the cap on the top half: `pad_to_separator` truncates to it. The left
/// half above is `blank + clock 3`, with the workspace strip beside the clock rather
/// than under it - four rows, which leaves eight below: volume 2, a blank, and the
/// player's five (title, artist, album, a blank, the transport), the art's frame
/// standing beside those five.
///
/// It does not line up with the right pane's, and that is the point: the left pane's
/// divider says what belongs with what, and volume belongs with the track.
const LEFT_SEPARATOR_ROW: usize = 4;

/// The same, for the right pane. The network pane shows one interface and needs five
/// rows for it - identity, link, rx, tx, selector.
///
/// It leaves the temperature panel six rows: two trend rows and a four-row grid. Over
/// two columns that is at most eight readings below the trends, and fewer when a group
/// cannot be split evenly, since a group never straddles the columns. A fifth grid row
/// is cut by `pad_to_separator` without saying so.
const RIGHT_SEPARATOR_ROW: usize = 7;

fn draw_left(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    mode: Mode,
    hits: &mut HitMap,
) {
    let time = state.now.format(&state.config.clock.format).to_string();
    /// Columns the block clock is indented by. The pane's left margin, shared with the
    /// divider, the mute buttons, the track title and the power row - and with the
    /// plain clock below, which has always used it.
    ///
    /// Not the volume bar's column: that one is `INDENT + icon_width + GAP`, which
    /// lands on 5 only while a Nerd Font makes the icon one column wide. Lining the
    /// clock up with it would come apart in the ASCII fallback.
    const CLOCK_INDENT: u16 = 2;

    let block = state.config.clock.style == ClockStyle::Block
        && mode == Mode::TwoPane
        && area.height >= clock::GLYPH_HEIGHT as u16 + 4
        && area.width >= clock::width(&time) + CLOCK_INDENT + 4;

    /// Columns between the clock and the workspace strip beside it.
    const CLOCK_GAP: u16 = 2;

    let mut flow = Flow::new(theme, area);

    // The date is on the top border, directly above the digits, so this row is blank.
    flow.blank();

    // The strip goes in the columns beside the clock rather than under it, on the middle
    // of the clock's three rows. Under the clock it takes three rows of its own, and
    // those are the rows the track's title, artist and album are drawn in. Pills fit
    // where three rows of mini-map never could: 39 columns is ten of them.
    let strip = workspaces::WorkspacesSlot {
        workspaces: &state.workspaces,
        count: state.config.workspaces.count,
        clickable: state.config.input.click_workspace,
        strip: workspaces::Strip::Pills,
    };
    let clock_columns = if block {
        clock::width(&time)
    } else {
        crate::util::text::width(&time) as u16
    };
    let strip_x = area.x + CLOCK_INDENT + clock_columns + CLOCK_GAP;
    // The strip keeps the pane's margin on its right, as the rows below it do.
    let strip_columns = area.right().saturating_sub(strip_x + INDENT);
    // The clock's last row, so the strip's bar ends where the digits do.
    let strip_row = area.y
        + flow.lines.len() as u16
        + if block {
            clock::GLYPH_HEIGHT as u16 - 1
        } else {
            0
        };
    let strip = (strip_columns >= CLOCK_GAP)
        .then(|| flow.render(&strip, Rect::new(strip_x, strip_row, strip_columns, 1)));
    let (strip_spans, strip_hits) = match strip {
        Some(rendered) => (
            rendered.lines.into_iter().next().map(|line| line.spans),
            rendered.hits,
        ),
        None => (None, Vec::new()),
    };
    flow.hits.extend(strip_hits);

    // The strip is appended to the clock's row rather than drawn over it: a pane is a
    // run of whole lines, and two widgets in one row would be two of them.
    let beside_clock = |row: String, style: Style, carries_strip: bool| {
        let mut spans = vec![Span::styled(
            format!("{}{row}", " ".repeat(CLOCK_INDENT as usize)),
            style,
        )];
        if carries_strip && let Some(strip) = strip_spans.clone() {
            spans.push(Span::raw(" ".repeat(CLOCK_GAP as usize)));
            spans.extend(strip);
        }
        Line::from(spans)
    };

    let accent = Style::default().fg(theme.part(Part::Clock).accent);
    if block {
        let last = clock::GLYPH_HEIGHT - 1;
        for (i, row) in clock::render(&time).into_iter().enumerate() {
            let line = beside_clock(row, accent, i == last);
            flow.push(line);
        }
    } else {
        let line = beside_clock(time.clone(), accent.add_modifier(Modifier::BOLD), true);
        flow.push(line);
    }

    // The divider goes straight under the clock, which puts volume and the track on the
    // same side of it: both are the sound coming out of this machine, and the one thing
    // above is what time it is and where you are.
    flow.pad_to_separator(LEFT_SEPARATOR_ROW);

    flow.place(&audio::AudioSlot {
        audio: &state.audio,
    });
    // Without this the input bar and the track's title touch, and the volume rows read
    // as the top of the track block rather than as their own.
    flow.blank();

    // The art's frame stands against the pane's margin, beside all five rows below the
    // divider, and those rows move over by the frame and a margin's worth of gap. With
    // no player there is no art and no frame either, so the rows keep their own margin
    // rather than standing off an empty gutter.
    let art = state.config.art.enabled && state.media.latest.is_some() && flow.left() >= ART_ROWS;
    let art_frame = Rect::new(
        area.x + INDENT,
        area.y + flow.lines.len() as u16,
        ART_COLUMNS,
        ART_ROWS,
    );
    let beside = if art { INDENT + ART_COLUMNS } else { 0 };

    flow.place_indented(
        &media::MediaSlot {
            media: &state.media,
        },
        beside,
    );

    flow.finish(frame, hits);

    if art {
        // Under the art's own window, so it shows only while there is no art to show.
        write_str(
            frame.buffer_mut(),
            art_frame.x + ART_COLUMNS / 2 - 1,
            art_frame.y + ART_ROWS / 2,
            theme.icon_note(),
            Style::default().fg(theme.part(Part::Media).dim),
        );
        hits.art = Some(art_frame);
    }
}

/// Columns the album art's frame takes. Over five rows of cells a little over twice as
/// tall as they are wide, ten comes out close to square.
const ART_COLUMNS: u16 = 10;

/// Rows the album art's frame takes: the five the player has below the divider - title,
/// artist, album, the blank under them, and the transport.
const ART_ROWS: u16 = 5;

fn draw_right(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme, hits: &mut HitMap) {
    let mut flow = Flow::new(theme, area);
    flow.blank();
    flow.place(&sensors::SensorSlot {
        sensors: &state.sensors,
        config: &state.config.sensors,
        history_len: state.config.general.history_len,
        columns: state.config.sensors.columns,
    });
    flow.pad_to_separator(RIGHT_SEPARATOR_ROW);
    // What is left below the divider is the network's budget - four rows at the
    // reference size, which is less than two interfaces.
    flow.place(&network::NetworkSlot {
        network: &state.network,
        history_len: state.config.general.history_len,
    });
    flow.finish(frame, hits);
}

/// Everything in one column, for a terminal too narrow for two panes.
///
/// The blocks are the same ones the two panes draw; what changes is that they no
/// longer all fit, so the layout has to choose. The temperatures drop to a single
/// column, since half of 70 columns is not enough for two.
pub fn draw_one_pane(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    theme: &Theme,
    hits: &mut HitMap,
) {
    if area.width < 8 || area.height < 4 {
        super::draw_placeholder(frame, area, state, theme, "tiny");
        return;
    }
    let border = Style::default().fg(theme.border);
    let pad = theme.frame.pad();
    frame.render_widget(
        Block::default()
            .borders(match theme.frame {
                FrameStyle::Full => Borders::ALL,
                FrameStyle::Top => Borders::TOP,
            })
            .border_style(border),
        area,
    );
    {
        let buf = frame.buffer_mut();
        write_date(buf, area, state, theme);
    }

    let inner = Rect::new(
        area.x + pad,
        area.y + 1,
        area.width - 2 * pad,
        area.height - 1 - pad,
    );
    let time = state.now.format(&state.config.clock.format).to_string();

    let workspaces = workspaces::WorkspacesSlot {
        workspaces: &state.workspaces,
        count: state.config.workspaces.count,
        clickable: state.config.input.click_workspace,
        // One column has the width for the whole mini-map, and no clock to sit beside.
        strip: workspaces::Strip::Map,
    };
    let audio_block = audio::AudioSlot {
        audio: &state.audio,
    };
    let sensors_block = sensors::SensorSlot {
        sensors: &state.sensors,
        config: &state.config.sensors,
        history_len: state.config.general.history_len,
        // Half of 70 columns cannot hold two readings side by side.
        columns: 1,
    };
    let network_block = network::NetworkSlot {
        network: &state.network,
        history_len: state.config.general.history_len,
    };
    let media_block = media::MediaSlot {
        media: &state.media,
    };
    let power_block = power::PowerSlot {
        ui: &state.ui,
        switch_error: state.workspaces.switch_error.as_deref(),
    };

    // The clock is not a slot: whether it is drawn as digits or as a block depends on
    // the rows the pane has left, which is the one thing a slot is not told.
    let clock_rows = Measure::fixed(1, slot::priority::CLOCK);
    // Reading order, which is the two panes' order read down one column. What gets
    // dropped is decided by priority, not by where a block sits.
    let blocks: [(&dyn Slot, u16); 5] = [
        // The map lines up under the clock's digits, as it does in the left pane.
        (&workspaces, INDENT),
        (&audio_block, 0),
        (&media_block, 0),
        (&sensors_block, 0),
        (&network_block, 0),
    ];
    let mut measures = vec![clock_rows];
    measures.extend(
        blocks
            .iter()
            .map(|(block, indent)| block.measure(inner.width.saturating_sub(*indent))),
    );
    measures.push(power_block.measure(inner.width));

    let rows = slot::allocate(&measures, inner.height);

    let mut flow = Flow::new(theme, inner);
    if rows[0] > 0 {
        flow.push(Line::styled(
            format!("  {time}"),
            Style::default()
                .fg(theme.part(Part::Clock).accent)
                .add_modifier(Modifier::BOLD),
        ));
    }
    for ((block, indent), rows) in blocks.iter().zip(&rows[1..]) {
        flow.place_in(*block, *rows, *indent);
    }
    // The power row sits on the last line of the pane, not straight after the block
    // above it: a status line that floats halfway up reads as part of what it follows.
    let power_rows = rows[rows.len() - 1];
    if power_rows > 0 {
        while flow.lines.len() + usize::from(power_rows) < usize::from(inner.height) {
            flow.blank();
        }
        flow.place_in(&power_block, power_rows, 0);
    }
    flow.finish(frame, hits);
}
