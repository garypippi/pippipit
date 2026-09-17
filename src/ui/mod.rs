//! The root layout.
//!
//! The reference size is 145x18, degrading in steps as the terminal shrinks.

pub mod audio;
pub mod bar;
pub mod clock;
pub mod confirm;
pub mod draw;
pub mod help;
pub mod hit;
pub mod media;
pub mod network;
pub mod pane;
pub mod power;
pub mod sensors;
pub mod slot;
pub mod theme;
pub mod workspaces;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::config::{FrameStyle, LayoutSetting};
use crate::state::AppState;
use hit::HitMap;
use theme::Theme;

/// The display mode, decided by the terminal size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The reference: two panes plus the power row.
    TwoPane,
    /// Not enough height; the clock drops to a single line.
    TwoPaneNarrow,
    /// Not enough width; stack vertically.
    OnePane,
    /// One to three rows, waybar style.
    Bar,
}

impl Mode {
    /// Anything but `auto` in the config pins the mode and ignores the terminal size.
    pub fn resolve(area: Rect, setting: LayoutSetting, frame: FrameStyle) -> Self {
        match setting {
            LayoutSetting::Auto => Mode::for_size(area, frame),
            LayoutSetting::TwoPane => Mode::TwoPane,
            LayoutSetting::TwoPaneNarrow => Mode::TwoPaneNarrow,
            LayoutSetting::OnePane => Mode::OnePane,
            LayoutSetting::Bar => Mode::Bar,
        }
    }

    /// The thresholds are about the rows the panes get, not the rows the terminal has:
    /// a frame without a bottom rule hands one back, so every row threshold comes down
    /// by one with it. The columns are unaffected - two columns either way is noise
    /// beside 130.
    pub fn for_size(area: Rect, frame: FrameStyle) -> Self {
        let chrome = frame.pad();
        let rows = area.height + 1 - chrome;
        match (area.width, rows) {
            (w, h) if w >= 130 && h >= 18 => Mode::TwoPane,
            (w, h) if w >= 130 && h >= 12 => Mode::TwoPaneNarrow,
            (_, h) if h >= 6 => Mode::OnePane,
            _ => Mode::Bar,
        }
    }
}

pub fn draw(frame: &mut Frame, state: &AppState, hits: &mut HitMap) {
    hits.clear();

    let theme = Theme::from_config(&state.config.theme);
    let area = frame.area();
    let mode = Mode::resolve(area, state.config.general.layout, theme.frame);

    match mode {
        Mode::TwoPane | Mode::TwoPaneNarrow => {
            pane::draw_two_pane(frame, area, state, &theme, mode, hits)
        }
        Mode::OnePane => pane::draw_one_pane(frame, area, state, &theme, hits),
        Mode::Bar => bar::draw(frame, area, state, &theme),
    }

    if state.ui.show_help {
        help::draw(frame, area, &theme);
    }

    // The confirmation modal is drawn last. Later hit-table entries win, so it reliably covers what is below.
    if let Some(action) = state.ui.pending_power {
        confirm::draw(
            frame,
            area,
            action,
            state.ui.confirm_yes_focused,
            &theme,
            hits,
        );
    }

    // The art is a window laid over the terminal, so it would cover an overlay rather
    // than be covered by it.
    if state.ui.show_help || state.ui.pending_power.is_some() {
        hits.art = None;
    }
}

fn draw_placeholder(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme, label: &str) {
    let time = state.now.format(&state.config.clock.format).to_string();
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!(" {time}  [{label}]"),
            Style::default().fg(theme.accent),
        )),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// A fixed state for the layout tests.
    ///
    /// **The layout tests must never read the live sources.** Doing so makes them
    /// depend on whatever hardware happens to be present, and puts this machine's
    /// interface names, addresses, SSID and device models into the test output.
    /// See `dump_live_render` for the deliberate live variant.
    pub(super) fn sample_state() -> AppState {
        use crate::sources::audio::{Audio, Device};
        use crate::sources::hyprland::{MonitorRow, Tile, WorkspaceCell, Workspaces};
        use crate::sources::media::{Media, Status};
        use crate::sources::network::{Interface, Kind, Network, Wireless};
        use crate::sources::sensors::Reading;
        use crate::util::sparkline::Ring;

        let mut state = AppState::new(Config::default());

        state.audio.latest = Audio {
            sink: Some(Device {
                description: "Speakers".into(),
                volume: 41,
                muted: false,
            }),
            source: Some(Device {
                description: "Microphone".into(),
                volume: 60,
                muted: false,
            }),
        };

        // Two side by side on 1, one filling 2, a dwindle three on 5.
        let split = |n: usize| -> Vec<Tile> {
            match n {
                1 => vec![Tile {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }],
                2 => vec![
                    Tile {
                        x: 0.0,
                        y: 0.0,
                        width: 0.5,
                        height: 1.0,
                    },
                    Tile {
                        x: 0.5,
                        y: 0.0,
                        width: 0.5,
                        height: 1.0,
                    },
                ],
                _ => vec![
                    Tile {
                        x: 0.0,
                        y: 0.0,
                        width: 0.5,
                        height: 1.0,
                    },
                    Tile {
                        x: 0.5,
                        y: 0.0,
                        width: 0.5,
                        height: 0.5,
                    },
                    Tile {
                        x: 0.5,
                        y: 0.5,
                        width: 0.5,
                        height: 0.5,
                    },
                ],
            }
        };
        let cells = |ids: &[(i64, usize)]| -> Vec<WorkspaceCell> {
            ids.iter()
                .map(|(id, windows)| WorkspaceCell {
                    id: *id,
                    occupied: *windows > 0,
                    tiles: split(*windows),
                })
                .collect()
        };
        state.workspaces.latest = Workspaces {
            monitors: vec![
                MonitorRow {
                    name: "DP-1".into(),
                    active: 5,
                    focused: true,
                    workspaces: cells(&[(1, 2), (2, 1), (5, 3)]),
                },
                MonitorRow {
                    name: "DP-2".into(),
                    active: 10,
                    focused: false,
                    workspaces: cells(&[(10, 1)]),
                },
                MonitorRow {
                    name: "HDMI-A-1".into(),
                    active: 9,
                    focused: false,
                    workspaces: cells(&[(9, 2)]),
                },
            ],
        };

        let reading = |group: &str, label: &str, celsius: f32| Reading {
            key: format!("{group}/{label}"),
            group: group.into(),
            label: label.into(),
            celsius,
            warn: 75.0,
            crit: 90.0,
        };
        state.sensors.latest = vec![
            reading("CPU", "Tctl", 40.0),
            reading("GPU", "edge", 45.0),
            reading("GPU", "junction", 59.0),
            reading("GPU", "mem", 67.0),
            reading("WiFi", "wifi", 52.0),
            reading("NVMe", "Composite", 35.0),
            reading("MB", "CPU", 45.0),
            reading("MB", "Motherboard", 37.0),
            reading("MB", "VRM", 46.0),
        ];
        for r in &state.sensors.latest {
            let ring = state
                .sensors
                .history
                .entry(r.key.clone())
                .or_insert_with(|| Ring::new(state.config.general.history_len));
            for offset in [-1.0, 0.5, -0.5, 1.0, 0.0] {
                ring.push(r.celsius + offset);
            }
        }

        state.media.latest = Some(Media {
            player: "firefox".into(),
            status: Status::Playing,
            position_us: 151_000_000,
            length_us: 304_000_000,
            title: "Sample Track".into(),
            artist: "Sample Artist".into(),
            album: "Sample Album".into(),
            art_url: String::new(),
        });

        state.network.latest = Network {
            interfaces: vec![Interface {
                kind: Kind::Wireless,
                name: "wlan0".into(),
                operstate: "up".into(),
                ipv4: Some("192.168.1.10/24".into()),
                wireless: Some(Wireless {
                    ssid: Some("MyNetwork-5G".into()),
                    freq_mhz: Some(5180),
                    key_mgmt: Some("WPA2-PSK".into()),
                    cipher: Some("CCMP".into()),
                    rssi: Some(-50),
                    link_mbps: Some(780),
                    state: Some("COMPLETED".into()),
                }),
                rx_bytes: 0,
                tx_bytes: 0,
            }],
        };

        state
    }

    fn render_with_hits(width: u16, height: u16) -> (String, HitMap, AppState) {
        let state = sample_state();
        let mut hits = HitMap::default();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, &state, &mut hits)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let text = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        (text, hits, state)
    }

    /// A fixed moment, so the date the tests look for is the date that gets drawn.
    fn set_now(state: &mut AppState, y: i32, m: u32, d: u32) {
        use chrono::{Local, TimeZone};
        state.now = Local.with_ymd_and_hms(y, m, d, 16, 26, 31).unwrap();
    }

    /// The headings said which program is running and that system readings are system
    /// readings, in the loudest style the palette has. The date says something.
    #[test]
    fn the_top_border_carries_the_date_and_not_the_headings() {
        let mut state = sample_state();
        set_now(&mut state, 2026, 8, 27);
        let top = render_state(&state, 145, 18)
            .lines()
            .next()
            .unwrap()
            .to_string();

        assert!(top.contains("2026-08-27"), "{top}");
        assert!(top.contains("Thu"), "{top}");
        assert!(!top.contains("pippipit"), "{top}");
        assert!(!top.contains("System"), "{top}");
    }

    /// The date belongs to the digits under it. One column out and the two read as
    /// unrelated.
    #[test]
    fn the_date_starts_on_the_clock_s_left_edge() {
        let mut state = sample_state();
        set_now(&mut state, 2026, 8, 27);
        let text = render_state(&state, 145, 18);
        let rows: Vec<&str> = text.lines().collect();

        // Columns, not byte offsets: a box-drawing character is three bytes, and the
        // border row starts with two of them where a glyph row starts with one.
        let first_ink = |row: &str| {
            row.chars()
                .position(|c| !matches!(c, ' ' | '│' | '─' | '┌' | '┬' | '┐'))
        };
        let date_at = rows[0].find("2026-08-27").expect("the date is drawn");
        let date_col = rows[0][..date_at].chars().count();
        // Rows 2..=4 are the three glyph rows; the blank row 1 has no ink.
        let clock_col = (2..=4)
            .filter_map(|r| first_ink(rows[r]))
            .min()
            .expect("the block clock is drawn");

        assert_eq!(
            date_col, clock_col,
            "date on row 0, clock on rows 2..=4:\n{text}"
        );
    }

    /// `weekday_format = ""` is how the weekday is turned off, so it must not leave a
    /// stray separator behind.
    #[test]
    fn an_empty_weekday_format_leaves_only_the_date() {
        let mut state = sample_state();
        set_now(&mut state, 2026, 8, 27);
        state.config.clock.weekday_format = String::new();
        let top = render_state(&state, 145, 18)
            .lines()
            .next()
            .unwrap()
            .to_string();

        assert!(top.contains(" 2026-08-27 "), "one space each side: {top}");
        assert!(!top.contains("Thu"), "{top}");
    }

    /// The volume bar's hit area must sit on the columns the bar is actually drawn in.
    /// If it drifts, the click position and the resulting volume disagree.
    #[test]
    fn volume_bar_hit_area_matches_the_drawn_bar() {
        use crate::ui::hit::Action;
        let (text, hits, _) = render_with_hits(145, 18);

        // Find where the bar really is on screen. `░` is the one glyph only the bar
        // uses - the block clock draws `█` too - and the reading is now drawn *inside*
        // the bar, so the row's first block glyph is the bar's first column.
        let (row, col) = text
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains('░'))
            .find_map(|(y, line)| {
                line.chars()
                    .position(|c| ['█', '▍', '░'].contains(&c))
                    .map(|col| (y as u16, col as u16))
            })
            .expect("volume bar should be drawn");

        match hits.at(col, row) {
            Some(Action::VolumeBar { target, x, width }) => {
                assert_eq!(target, crate::ui::hit::AudioTarget::Sink, "output is first");
                assert_eq!(x, col, "hit area starts where the bar is drawn");
                assert_eq!(width as usize, super::audio::BAR_WIDTH);
                // The bar's rightmost column hits; one past it does not.
                assert!(hits.at(x + width - 1, row).is_some());
                assert!(hits.at(x + width, row).is_none());
            }
            other => panic!("expected VolumeBar at ({col},{row}), got {other:?}"),
        }

        // The input row underneath is the same shape, and is its own device.
        assert!(
            matches!(
                hits.at(col, row + 1),
                Some(Action::VolumeBar {
                    target: crate::ui::hit::AudioTarget::Source,
                    ..
                })
            ),
            "the input row must be a control too, not a readout"
        );
    }

    /// Where the workspace strip sits on screen, found through the hit map.
    ///
    /// Not by what is drawn: "equal-width groups" also match **the clock**, whose block
    /// digits are groups of a fixed width too, but only at the seconds where every
    /// digit's top row is solid - a test that fails a few times a minute. The hit map
    /// does not depend on what is drawn at all.
    ///
    /// Returns the column and row of slot 1's top-left corner.
    fn workspace_strip(hits: &HitMap, width: u16, height: u16) -> Option<(u16, u16)> {
        use crate::ui::hit::Action;
        (0..height).find_map(|y| {
            (0..width)
                .find(|x| hits.at(*x, y) == Some(Action::Workspace { id: 1 }))
                .map(|x| (x, y))
        })
    }

    /// Without the numbers, position is the only thing that says which workspace a
    /// slot is - so every slot has to survive, at the narrowest 2-pane too (130
    /// columns, a 64-column left pane).
    #[test]
    fn every_workspace_slot_survives_a_narrow_pane() {
        use crate::ui::hit::Action;
        let stride = super::workspaces::PILL_STRIDE;
        for width in [145u16, 130] {
            let (text, hits, _) = render_with_hits(width, 18);
            let (first, row) = workspace_strip(&hits, width, 18)
                .unwrap_or_else(|| panic!("no workspace strip at {width}:\n{text}"));
            for slot in 0..10u16 {
                let x = first + slot * stride;
                assert_eq!(
                    hits.at(x, row),
                    Some(Action::Workspace {
                        id: slot as i64 + 1
                    }),
                    "slot {slot} at column {x}, {width} columns:\n{text}"
                );
            }
        }
    }

    /// The strip is drawn as part of a paragraph but registers its hit areas against a
    /// rect worked out at the call site. If the two drift apart, a click lands on the
    /// wrong workspace - or on nothing.
    #[test]
    fn workspace_hit_areas_sit_on_the_slots_as_drawn() {
        use crate::ui::hit::Action;
        let (text, hits, _) = render_with_hits(145, 18);
        let lines: Vec<Vec<char>> = text.lines().map(|line| line.chars().collect()).collect();
        let (first, top) = workspace_strip(&hits, 145, 18)
            .unwrap_or_else(|| panic!("no workspace strip:\n{text}"));
        let (columns, stride) = (
            super::workspaces::PILL_COLUMNS,
            super::workspaces::PILL_STRIDE,
        );

        for slot in 0..10u16 {
            let x = first + slot * stride;
            let want = Some(Action::Workspace {
                id: slot as i64 + 1,
            });
            // The whole pill is the target, blank column included: an empty workspace
            // draws a dot and a space, and the space has to be clickable too.
            for column in x..x + columns {
                assert_eq!(hits.at(column, top), want, "slot {slot} at {column},{top}");
            }
            // The gap belongs to the pill on its left, which is what makes a two-column
            // target hittable. The last pill has no gap to take.
            if slot + 1 < 10 {
                assert_eq!(hits.at(x + columns, top), want, "the gap after slot {slot}");
            }
            // And something is actually drawn inside those columns, or the target is
            // over a pill with nothing to aim at.
            let drawn = (x..x + columns).any(|column| lines[top as usize][column as usize] != ' ');
            assert!(drawn, "slot {slot} draws nothing at column {x}:\n{text}");
        }
        // The strip is one row: the clock's rows either side of it take no clicks.
        assert_eq!(hits.at(first, top + 1), None, "below the strip");
        assert_eq!(hits.at(first, top - 1), None, "above the strip");
    }

    fn render_at(width: u16, height: u16) -> String {
        render_state(&sample_state(), width, height)
    }

    fn render_state(state: &AppState, width: u16, height: u16) -> String {
        let mut hits = HitMap::default();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, state, &mut hits)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The reference pane size, drawn from the fixed sample state.
    /// Eyeball it with `cargo test -- --nocapture renders_at_real_pane_size`.
    #[test]
    fn renders_at_real_pane_size() {
        let out = render_at(145, 18);
        println!("{out}");
        for (i, line) in out.lines().enumerate() {
            assert_eq!(line.chars().count(), 145, "line {i} width");
        }
        assert_eq!(out.lines().count(), 18);
    }

    /// A block's own colour reaches only that block: the clock's accent is on the
    /// digits and nowhere else, and the trends follow `sparkline` rather than it.
    #[test]
    fn a_block_colour_stays_in_its_block() {
        use ratatui::style::Color;
        let mut state = sample_state();
        state.config.theme.clock.accent = Some(crate::config::ColorName("magenta".into()));
        state.config.theme.sparkline = Some(crate::config::ColorName("blue".into()));
        let mut hits = HitMap::default();
        let mut terminal = Terminal::new(TestBackend::new(145, 18)).unwrap();
        terminal.draw(|f| draw(f, &state, &mut hits)).unwrap();
        let b = terminal.backend().buffer();
        let cells_in = |colour: Color| -> Vec<(u16, u16)> {
            (0..b.area.height)
                .flat_map(|y| (0..b.area.width).map(move |x| (x, y)))
                .filter(|&(x, y)| b[(x, y)].fg == colour && b[(x, y)].symbol() != " ")
                .collect()
        };

        let magenta = cells_in(Color::Magenta);
        assert!(!magenta.is_empty(), "the clock takes its own accent");
        // The block clock is on screen rows 2-4, under the frame and the blank row,
        // and left of the workspace strip.
        assert!(
            magenta.iter().all(|&(x, y)| (2..=4).contains(&y) && x < 40),
            "{magenta:?}"
        );
        assert!(!cells_in(Color::Blue).is_empty(), "the trends are blue");
        assert!(
            !cells_in(Color::Cyan).is_empty(),
            "everything else keeps the accent"
        );
    }

    /// The same panes with `frame = "top"`, in **one row fewer**.
    ///
    /// This is the whole point of the setting: the sides and the bottom rule are a
    /// second box around a window Hyprland has already drawn a border and a gap
    /// around, and giving them up buys back the row the panes were short of. The date
    /// stays where it was, because the top rule stays.
    ///
    /// Eyeball it with `cargo test -- --nocapture renders_with_a_top_only_frame`.
    #[test]
    fn renders_with_a_top_only_frame() {
        let mut state = sample_state();
        set_now(&mut state, 2026, 8, 27);
        state.config.theme.frame = FrameStyle::Top;
        let out = render_state(&state, 145, 17);
        println!("{out}");
        for (i, line) in out.lines().enumerate() {
            assert_eq!(line.chars().count(), 145, "line {i} width");
        }
        let rows: Vec<&str> = out.lines().collect();
        assert_eq!(rows.len(), 17);

        // Seventeen rows is a two-pane layout with this frame, and one row short of it
        // with the full one.
        assert_eq!(
            Mode::for_size(Rect::new(0, 0, 145, 17), FrameStyle::Top),
            Mode::TwoPane
        );
        assert_eq!(
            Mode::for_size(Rect::new(0, 0, 145, 17), FrameStyle::Full),
            Mode::TwoPaneNarrow
        );

        // No column of the frame on either side, and no bottom rule.
        assert!(!rows[1].starts_with('│'), "{:?}", rows[1]);
        assert!(!rows[1].ends_with('│'), "{:?}", rows[1]);
        assert!(!rows[16].starts_with('└'), "{:?}", rows[16]);

        // The date still sits on the top rule, over the clock's left edge.
        assert!(rows[0].contains("2026-08-27"), "{:?}", rows[0]);
        let column = |line: &str, byte: usize| line[..byte].chars().count();
        let date = column(rows[0], rows[0].find("2026").unwrap());
        let clock = column(rows[2], rows[2].find(|c: char| c != ' ').unwrap());
        assert_eq!(date, clock, "the date starts on the clock's left edge");

        // The rule above the power row runs edge to edge, with the cross still in it.
        assert!(rows[15].starts_with('─'), "{:?}", rows[15]);
        assert!(rows[15].ends_with('─'), "{:?}", rows[15]);
        assert!(rows[15].contains('┴'), "{:?}", rows[15]);
    }

    /// The single stacked pane, on a terminal too narrow for two.
    ///
    /// Everything the two panes draw is here, in reading order, with the temperatures
    /// down to one column. Eyeball it with `cargo test -- --nocapture renders_one_pane`.
    #[test]
    fn renders_one_pane() {
        let mut state = sample_state();
        set_now(&mut state, 2026, 8, 27);
        let out = render_state(&state, 80, 20);
        println!("{out}");
        assert_eq!(
            Mode::for_size(Rect::new(0, 0, 80, 20), FrameStyle::Full),
            Mode::OnePane
        );
        for (i, line) in out.lines().enumerate() {
            assert_eq!(line.chars().count(), 80, "line {i} width");
        }
        // The clock, the map, both volume rows, the track, the readings, the network
        // and the power row: everything, because at this size everything fits.
        assert!(out.contains("16:26:31"), "the clock");
        assert!(out.contains("Speakers"), "the output device");
        assert!(out.contains("Sample Track"), "the track");
        assert!(out.contains("Tctl"), "the readings");
        assert!(out.contains("192.168"), "the address");
        assert!(
            out.contains(
                Theme::default().icon_power_action(crate::sources::power::PowerAction::PowerOff)
            ),
            "the power row"
        );
        // The power row is on the last line inside the frame, not floating mid-pane.
        let rows: Vec<&str> = out.lines().collect();
        assert!(
            rows[rows.len() - 2].contains(
                Theme::default().icon_power_action(crate::sources::power::PowerAction::ScreenOff)
            ),
            "{:?}",
            rows
        );
    }

    /// The bar, which is one line whatever height it is given.
    #[test]
    fn renders_bar() {
        let mut state = sample_state();
        set_now(&mut state, 2026, 8, 27);
        let out = render_state(&state, 60, 3);
        println!("{out}");
        assert_eq!(
            Mode::for_size(Rect::new(0, 0, 60, 3), FrameStyle::Full),
            Mode::Bar
        );
        assert_eq!(out.lines().count(), 3);
        let line = out.lines().nth(1).expect("the middle row");
        assert!(line.contains("16:26:31"), "the clock survives: {line:?}");
        // The focused workspace is bracketed, which is the only mark it gets here.
        assert!(line.contains('['), "the focused workspace: {line:?}");
    }

    /// What the bar gives up as it narrows, and the order it gives it up in.
    #[test]
    fn the_bar_drops_the_weakest_first() {
        let mut state = sample_state();
        set_now(&mut state, 2026, 8, 27);

        let wide = render_state(&state, 145, 1);
        assert!(wide.contains("Sample Track"), "the track fits: {wide:?}");
        assert!(wide.contains("CPU 40°C"), "and the readings: {wide:?}");

        let narrow = render_state(&state, 70, 1);
        assert!(
            narrow.contains("16:26:31"),
            "the clock is last to go: {narrow:?}"
        );
        assert!(
            !narrow.contains("Sample Track"),
            "the track goes: {narrow:?}"
        );

        let tiny = render_state(&state, 24, 1);
        assert!(tiny.contains("16:26:31"), "still the clock: {tiny:?}");
    }

    /// The same frame, but filled from the machine this runs on.
    ///
    /// **Ignored by default, and it must stay that way.** It reads live sources,
    /// so its output carries whatever this machine exposes: interface names,
    /// addresses, SSID, device models. A plain `#[test]` would leak all of that
    /// into any CI log the moment an assertion here failed, which is exactly
    /// when it would be captured and published.
    ///
    /// Run it deliberately, and do not paste the output anywhere public:
    ///
    /// ```text
    /// cargo test -- --ignored --nocapture dump_live_render
    /// ```
    ///
    /// The assertions stay on the frame geometry only, since the contents
    /// differ per machine.
    #[test]
    #[ignore = "reads live sources; its output describes this machine"]
    fn dump_live_render() {
        let mut state = AppState::new(Config::default());
        state.refresh_sensors();
        state.refresh_audio();
        state.refresh_workspaces();
        state.refresh_network();
        let out = render_state(&state, 145, 18);
        println!("{out}");
        for (i, line) in out.lines().enumerate() {
            assert_eq!(line.chars().count(), 145, "line {i} width");
        }
    }

    #[test]
    fn mode_thresholds() {
        assert_eq!(
            Mode::for_size(Rect::new(0, 0, 145, 18), FrameStyle::Full),
            Mode::TwoPane
        );
        assert_eq!(
            Mode::for_size(Rect::new(0, 0, 141, 16), FrameStyle::Full),
            Mode::TwoPaneNarrow
        );
        assert_eq!(
            Mode::for_size(Rect::new(0, 0, 80, 20), FrameStyle::Full),
            Mode::OnePane
        );
        assert_eq!(
            Mode::for_size(Rect::new(0, 0, 60, 3), FrameStyle::Full),
            Mode::Bar
        );
    }

    /// Small sizes must neither panic nor overflow.
    #[test]
    fn layout_setting_overrides_the_size_heuristic() {
        let big = Rect::new(0, 0, 145, 18);
        assert_eq!(
            Mode::resolve(big, LayoutSetting::Auto, FrameStyle::Full),
            Mode::TwoPane
        );
        assert_eq!(
            Mode::resolve(big, LayoutSetting::Bar, FrameStyle::Full),
            Mode::Bar
        );
        assert_eq!(
            Mode::resolve(big, LayoutSetting::OnePane, FrameStyle::Full),
            Mode::OnePane
        );
        let tiny = Rect::new(0, 0, 40, 3);
        assert_eq!(
            Mode::resolve(tiny, LayoutSetting::Auto, FrameStyle::Full),
            Mode::Bar
        );
        assert_eq!(
            Mode::resolve(tiny, LayoutSetting::TwoPane, FrameStyle::Full),
            Mode::TwoPane
        );
    }

    #[test]
    fn survives_small_sizes() {
        for (w, h) in [(145, 18), (141, 16), (80, 20), (60, 3), (20, 2), (5, 1)] {
            let out = render_at(w, h);
            assert_eq!(out.lines().count(), h as usize, "{w}x{h}");
            for line in out.lines() {
                assert_eq!(line.chars().count(), w as usize, "{w}x{h}");
            }
        }
    }

    /// Pinning `layout` by hand allows combinations that do not match the terminal size.
    /// **None of them may panic or overflow the buffer.**
    #[test]
    fn forced_layout_never_overflows() {
        for setting in [
            LayoutSetting::Auto,
            LayoutSetting::TwoPane,
            LayoutSetting::TwoPaneNarrow,
            LayoutSetting::OnePane,
            LayoutSetting::Bar,
        ] {
            for (w, h) in [(145, 18), (130, 18), (60, 4), (20, 2), (8, 6), (5, 1)] {
                let mut state = AppState::new(Config::default());
                state.config.general.layout = setting;
                let mut hits = HitMap::default();
                let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
                terminal.draw(|f| draw(f, &state, &mut hits)).unwrap();
                let b = terminal.backend().buffer().clone();
                assert_eq!(b.area.width, w, "{setting:?} at {w}x{h}");
                assert_eq!(b.area.height, h, "{setting:?} at {w}x{h}");
            }
        }
    }
}

#[cfg(test)]
mod boot_tests {
    use super::*;
    use crate::config::Config;
    use crate::sources::network::{Interface, Kind, Network, Wireless};
    use crate::ui::hit::Action;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(state: &AppState) -> String {
        let mut hits = HitMap::default();
        let mut terminal = Terminal::new(TestBackend::new(145, 18)).unwrap();
        terminal.draw(|f| draw(f, state, &mut hits)).unwrap();
        let b = terminal.backend().buffer().clone();
        (0..b.area.height)
            .map(|y| {
                (0..b.area.width)
                    .map(|x| b[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn iface(name: &str, ip: &str, wireless: bool) -> Interface {
        Interface {
            // What `sources::network::kind` would decide, without touching /sys.
            kind: match name {
                _ if wireless => Kind::Wireless,
                n if n.starts_with("docker") || n.starts_with("veth") || n.starts_with("br-") => {
                    Kind::Virtual
                }
                _ => Kind::Wired,
            },
            name: name.into(),
            operstate: "up".into(),
            ipv4: Some(ip.into()),
            wireless: wireless.then(|| Wireless {
                ssid: Some("MyNetwork-5G".into()),
                freq_mhz: Some(5180),
                key_mgmt: Some("WPA2-PSK".into()),
                cipher: Some("CCMP".into()),
                rssi: Some(-51),
                link_mbps: Some(866),
                state: Some("COMPLETED".into()),
            }),
            rx_bytes: 0,
            tx_bytes: 0,
        }
    }

    /// Four interfaces at 145x18: the pane has four rows and the WiFi block alone is
    /// three of them.
    fn crowded(state: &mut AppState) {
        state.network.latest = Network {
            interfaces: vec![
                iface("wlp11s0", "192.168.1.42/24", true),
                iface("docker0", "172.17.0.1/16", false),
                iface("veth1a2b3c4", "10.0.0.1/8", false),
                iface("enp5s0", "192.168.1.9/24", false),
            ],
        };
    }

    /// Where the selector registered a given interface, if it did.
    fn select_action(hits: &HitMap, index: u8) -> Option<(u16, u16)> {
        (0..18u16)
            .flat_map(|y| (0..145u16).map(move |x| (x, y)))
            .find(|(x, y)| hits.at(*x, *y) == Some(Action::NetworkSelect { index }))
    }

    fn draw_once(state: &mut AppState) -> String {
        let mut hits = HitMap::default();
        let mut terminal = Terminal::new(TestBackend::new(145, 18)).unwrap();
        terminal.draw(|f| draw(f, state, &mut hits)).unwrap();
        state.ui.hits = hits;
        render(state)
    }

    fn click(state: &mut AppState, x: u16, y: u16) {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        state.reduce(crate::event::Event::Input(crossterm::event::Event::Mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        )));
    }

    /// The pane shows one interface, so the selector is the only thing that says what
    /// else the machine has - and it has to say so whether or not it can be clicked.
    #[test]
    fn the_selector_names_every_interface() {
        let mut state = AppState::new(Config::default());
        crowded(&mut state);
        let out = draw_once(&mut state);
        for name in ["wlp11s0", "docker0", "veth1a2b3c4", "enp5s0"] {
            assert!(out.contains(name), "{name} must be named:\n{out}");
        }
        // And the hardware is what is shown, because that is what `order` puts first.
        assert!(out.contains("192.168.1.42/24"), "{out}");
    }

    /// Clicking a name shows that interface.
    #[test]
    fn clicking_a_name_shows_that_interface() {
        let mut state = AppState::new(Config::default());
        crowded(&mut state);
        draw_once(&mut state);
        let (x, y) = select_action(&state.ui.hits, 3).expect("enp5s0 must be clickable");
        click(&mut state, x, y);
        assert_eq!(state.network.selected.as_deref(), Some("enp5s0"));
        let out = draw_once(&mut state);
        assert!(
            out.contains("192.168.1.9/24"),
            "enp5s0 must be shown:\n{out}"
        );
    }

    /// The whole reason the selection is a name. `order` re-sorts on every refresh, so
    /// a container starting would slide an index onto a different interface.
    #[test]
    fn the_selection_survives_the_list_being_reordered() {
        let mut state = AppState::new(Config::default());
        crowded(&mut state);
        draw_once(&mut state);
        let (x, y) = select_action(&state.ui.hits, 3).expect("enp5s0 must be clickable");
        click(&mut state, x, y);

        // A container appears and lands ahead of it in the list.
        state
            .network
            .latest
            .interfaces
            .insert(1, iface("br-9f2a", "10.1.0.1/16", false));
        let out = draw_once(&mut state);
        assert_eq!(state.network.selected.as_deref(), Some("enp5s0"));
        assert!(out.contains("192.168.1.9/24"), "still enp5s0:\n{out}");
    }

    /// A new interface must not steal the view from whatever is being looked at.
    #[test]
    fn a_new_interface_does_not_move_the_selection() {
        let mut state = AppState::new(Config::default());
        crowded(&mut state);
        draw_once(&mut state);
        let (x, y) = select_action(&state.ui.hits, 1).expect("docker0 must be clickable");
        click(&mut state, x, y);
        state
            .network
            .latest
            .interfaces
            .insert(0, iface("wlp0s1", "192.168.5.5/24", true));
        let out = draw_once(&mut state);
        assert!(out.contains("172.17.0.1/16"), "still docker0:\n{out}");
    }

    /// An interface that goes away falls back to the first in the current order, which
    /// `rank` has already made the most useful one.
    #[test]
    fn a_selection_that_disappears_falls_back_to_the_first() {
        let mut state = AppState::new(Config::default());
        crowded(&mut state);
        state.network.selected = Some("enp5s0".into());
        state
            .network
            .latest
            .interfaces
            .retain(|i| i.name != "enp5s0");
        let out = draw_once(&mut state);
        assert!(out.contains("192.168.1.42/24"), "the wireless one:\n{out}");
        assert!(!out.contains("no active interfaces"), "{out}");
    }

    /// A selector that hides what you are looking at has stopped being a selector.
    #[test]
    fn the_selected_interface_is_never_the_one_that_overflows() {
        let mut state = AppState::new(Config::default());
        let mut interfaces = vec![iface("wlp11s0", "192.168.1.42/24", true)];
        for i in 0..8 {
            interfaces.push(iface(&format!("veth{i}a2b3c4"), "10.0.0.1/8", false));
        }
        let last = interfaces.len() - 1;
        let last_name = interfaces[last].name.clone();
        state.network.latest = Network { interfaces };
        state.network.selected = Some(last_name.clone());
        let out = draw_once(&mut state);
        assert!(
            out.contains(&last_name),
            "the selected one must be listed:\n{out}"
        );
        assert!(
            out.contains(theme_more()),
            "the rest must be counted:\n{out}"
        );
    }

    fn theme_more() -> &'static str {
        Theme::default().icon_more()
    }

    /// One interface needs no selector, and the row is better spent on nothing than on
    /// a control with a single option.
    #[test]
    fn a_single_interface_needs_no_selector() {
        let mut state = AppState::new(Config::default());
        state.network.latest = Network {
            interfaces: vec![iface("enp5s0", "192.168.1.9/24", false)],
        };
        let out = draw_once(&mut state);
        assert!(out.contains("192.168.1.9/24"), "{out}");
        assert_eq!(select_action(&state.ui.hits, 0), None);
    }

    /// The wheel does nothing over the network pane, and must not reach the volume
    /// from over here either.
    #[test]
    fn the_wheel_does_nothing_over_the_network_pane() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut state = AppState::new(Config::default());
        crowded(&mut state);
        draw_once(&mut state);
        let before = state.network.selected.clone();
        state.reduce(crate::event::Event::Input(crossterm::event::Event::Mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 100,
                row: 13,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        )));
        assert_eq!(state.network.selected, before, "the wheel must not select");
        assert!(state.audio.error.is_none(), "and must not touch the volume");
    }

    /// Just after boot: wpa is still authenticating and there is no IP yet.
    /// **The row must survive and the state must be readable.**
    #[test]
    fn boot_state_shows_dormant_interface_without_address() {
        let mut state = AppState::new(Config::default());
        state.network.latest = Network {
            interfaces: vec![Interface {
                kind: Kind::Wireless,
                name: "wlan0".into(),
                operstate: "dormant".into(),
                ipv4: None,
                wireless: Some(Wireless {
                    ssid: None,
                    freq_mhz: None,
                    key_mgmt: None,
                    cipher: None,
                    rssi: None,
                    link_mbps: None,
                    state: Some("ASSOCIATING".into()),
                }),
                rx_bytes: 0,
                tx_bytes: 0,
            }],
        };
        let out = render(&state);
        assert!(
            out.contains("wlan0"),
            "interface row must stay visible:\n{out}"
        );
        assert!(out.contains("DORMANT"), "operstate must be visible:\n{out}");
        assert!(
            out.contains("no address yet"),
            "address state must be visible:\n{out}"
        );
        assert!(
            out.contains("ASSOCIATING"),
            "wpa state must be visible:\n{out}"
        );
    }

    /// The playing track must render **without escaping the left pane**.
    /// With only 14 rows, every added block threatens this.
    #[test]
    fn media_rows_fit_inside_the_left_pane() {
        use crate::sources::media::{Media, Status};
        let mut state = AppState::new(Config::default());
        state.media.latest = Some(Media {
            player: "firefox".into(),
            status: Status::Playing,
            position_us: 81_000_000,
            length_us: 265_000_000,
            title: "Sample Track".into(),
            artist: "Sample Artist".into(),
            album: "Sample Album".into(),
            art_url: String::new(),
        });
        let out = render(&state);
        assert!(
            out.contains("Sample Track"),
            "title must be visible:\n{out}"
        );
        assert!(
            out.contains("Sample Artist"),
            "artist must be visible:\n{out}"
        );
        assert!(out.contains("firefox"), "player must be visible:\n{out}");
        // The seek bar and the timestamps (`4:25` is 265 seconds).
        assert!(out.contains("4:25"), "length must be visible:\n{out}");
        assert!(out.contains('━'), "seek bar must be visible:\n{out}");
    }

    /// A row each for the title, the artist and the album. Packed onto one row they cost
    /// each other columns, and the title - the one field worth reading - was the one
    /// being ellipsised.
    #[test]
    fn the_track_gets_a_row_for_each_field() {
        let text = draw_into_hits(&mut super::tests::sample_state(), 145, 18);
        let row_of = |needle: &str| {
            text.lines()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("{needle} missing:\n{text}"))
        };
        let title = row_of("Sample Track");
        assert_eq!(row_of("Sample Artist"), title + 1);
        assert_eq!(row_of("Sample Album"), title + 2);
        // The player's name stays on the title's row, where the field it names starts.
        assert_eq!(row_of("firefox"), title);
    }

    /// With no player there is no art to hang beside the rows, so the frame goes and the
    /// rows keep the pane's own margin instead of standing off an empty gutter.
    #[test]
    fn no_media_gives_the_art_gutter_back() {
        let mut state = state_with_art();
        state.media.latest = None;
        let out = draw_into_hits(&mut state, 145, 18);
        assert_eq!(state.ui.hits.art, None, "no track, no frame");

        let row = out
            .lines()
            .find(|line| line.contains("no media"))
            .expect("the block says there is no player");
        let indent = row
            .chars()
            .skip(1) // the frame's own column
            .take_while(|c| *c == ' ')
            .count();
        assert!(
            indent < ART_FRAME.0 as usize,
            "indented by {indent}: {row:?}"
        );
    }

    /// Draw `state` and keep the hit table, as the event loop does.
    fn draw_into_hits(state: &mut AppState, width: u16, height: u16) -> String {
        let mut hits = HitMap::default();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, state, &mut hits)).unwrap();
        state.ui.hits = hits;
        let b = terminal.backend().buffer().clone();
        (0..b.area.height)
            .map(|y| {
                (0..b.area.width)
                    .map(|x| b[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn state_with_art() -> AppState {
        let mut state = super::tests::sample_state();
        state.config.art.enabled = true;
        if let Some(media) = state.media.latest.as_mut() {
            media.art_url = "file:///tmp/cover.png".into();
        }
        state
    }

    /// The frame takes the left of the five rows under the divider and the track rows
    /// move over for it, while the volume rows above the divider keep the pane's own
    /// margin - nothing stands beside them.
    #[test]
    fn the_art_frame_moves_the_track_rows_over() {
        let mut state = state_with_art();
        let out = draw_into_hits(&mut state, 145, 18);
        let hits = &state.ui.hits;
        let art = hits.art.expect("the reference size has room for the art");
        assert_eq!((art.width, art.height), (ART_FRAME.0, ART_FRAME.1));

        let right_of_art = art.x + art.width + 2;
        let first_at = |y: u16, wanted: fn(Action) -> bool| {
            (0..145).find(|x| hits.at(*x, y).is_some_and(wanted))
        };
        let previous = first_at(art.y + art.height - 1, |a| a == Action::Previous)
            .expect("the transport row is beside the art");
        assert!(previous >= right_of_art, "previous at {previous}");

        let mute = (0..18)
            .find_map(|y| first_at(y, |a| matches!(a, Action::ToggleMute { .. })).map(|x| (x, y)))
            .expect("a mute button");
        assert!(
            mute.1 < art.y,
            "the volume rows sit above the art, not beside it"
        );
        assert_eq!(mute.0, art.x, "and start in the frame's own column");

        assert!(
            out.contains("Speakers"),
            "device name must be visible:\n{out}"
        );
        assert!(out.contains("5:04"), "length must be visible:\n{out}");
    }

    /// The art is a window over the terminal: an overlay would be drawn under it.
    #[test]
    fn an_overlay_takes_the_art_away() {
        let mut state = state_with_art();
        state.ui.show_help = true;
        draw_into_hits(&mut state, 145, 18);
        assert_eq!(state.ui.hits.art, None);
    }

    /// Only the two-pane layout has the rows under the divider for it.
    #[test]
    fn the_art_needs_room_and_asking_for() {
        let mut state = state_with_art();
        draw_into_hits(&mut state, 145, 14);
        assert_eq!(state.ui.hits.art, None, "2-pane-narrow");
        draw_into_hits(&mut state, 80, 20);
        assert_eq!(state.ui.hits.art, None, "1-pane");

        let mut off = super::tests::sample_state();
        draw_into_hits(&mut off, 145, 18);
        assert_eq!(off.ui.hits.art, None, "art is off by default");
    }

    /// The provider hears about a placement once, and about the art going away.
    #[test]
    fn the_art_placement_is_sent_only_when_it_changes() {
        use crate::sources::art::ArtCommand;
        let (tx, rx) = std::sync::mpsc::channel();
        let mut state = state_with_art();
        state.art_tx = Some(tx);

        draw_into_hits(&mut state, 145, 18);
        state.place_art((145, 18));
        draw_into_hits(&mut state, 145, 18);
        state.place_art((145, 18));
        // A resize has to reach Überzug++ even when the frame's cells stay put.
        state.place_art((150, 18));
        state.media.latest = None;
        draw_into_hits(&mut state, 145, 18);
        state.place_art((145, 18));

        let sent: Vec<ArtCommand> = rx.try_iter().collect();
        assert!(
            matches!(
                sent.as_slice(),
                [ArtCommand::Show(_), ArtCommand::Show(_), ArtCommand::Hide]
            ),
            "{sent:?}"
        );
    }

    /// The frame's size, restated so a change to it is a change to this test too.
    const ART_FRAME: (u16, u16) = (10, 5);

    /// With `nerd_font = false`, no Nerd Font glyph may remain on screen.
    #[test]
    fn ascii_mode_removes_the_fancy_glyphs() {
        use crate::sources::media::{Media, Status};
        let mut state = AppState::new(Config::default());
        state.config.theme.nerd_font = false;
        state.media.latest = Some(Media {
            player: "firefox".into(),
            status: Status::Playing,
            position_us: 1_000_000,
            length_us: 10_000_000,
            title: "T".into(),
            artist: "A".into(),
            album: "B".into(),
            art_url: String::new(),
        });
        let mut hits = HitMap::default();
        let mut terminal = Terminal::new(TestBackend::new(145, 18)).unwrap();
        terminal.draw(|f| draw(f, &state, &mut hits)).unwrap();
        let b = terminal.backend().buffer().clone();
        let text: String = (0..b.area.height)
            .flat_map(|y| (0..b.area.width).map(move |x| (x, y)))
            .map(|(x, y)| b[(x, y)].symbol().to_string())
            .collect();

        for glyph in ['⏮', '⏭', '▶', '⏸', '⏹', '♪', '↓', '↑', '●'] {
            assert!(
                !text.contains(glyph),
                "{glyph} must not appear in ascii mode"
            );
        }
        assert!(text.contains(">"), "ascii play marker should be there");
    }

    /// The pending state must be visible; looking unresponsive invites a second press and an accident.
    #[test]
    fn pending_power_action_is_shown() {
        use crate::sources::power::PowerAction;
        let mut state = AppState::new(Config::default());
        state.ui.power_running = Some(PowerAction::Suspend);
        let mut hits = HitMap::default();
        let mut terminal = Terminal::new(TestBackend::new(145, 18)).unwrap();
        terminal.draw(|f| draw(f, &state, &mut hits)).unwrap();
        let b = terminal.backend().buffer().clone();
        let text: String = (0..b.area.height)
            .map(|y| {
                (0..b.area.width)
                    .map(|x| b[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Suspend…"), "must show progress:\n{text}");
    }

    /// Each power button's hit area must sit on the columns that button is drawn in,
    /// whichever font decides how wide those columns are.
    #[test]
    fn power_buttons_are_clickable_where_they_are_drawn() {
        use crate::sources::power::PowerAction;
        for nerd_font in [true, false] {
            let mut state = AppState::new(Config::default());
            state.config.theme.nerd_font = nerd_font;
            let theme = Theme::from_config(&state.config.theme);
            let mut hits = HitMap::default();
            let mut terminal = Terminal::new(TestBackend::new(145, 18)).unwrap();
            terminal.draw(|f| draw(f, &state, &mut hits)).unwrap();
            let b = terminal.backend().buffer().clone();
            let text: Vec<String> = (0..b.area.height)
                .map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect())
                .collect();

            for action in [
                PowerAction::ScreenOff,
                PowerAction::Suspend,
                PowerAction::PowerOff,
            ] {
                let glyph = theme.icon_power_action(action);
                let (row, col) = text
                    .iter()
                    .enumerate()
                    .find_map(|(y, line)| {
                        line.find(glyph)
                            .map(|i| (y as u16, line[..i].chars().count() as u16))
                    })
                    .unwrap_or_else(|| panic!("{glyph} should be drawn"));
                assert_eq!(
                    hits.at(col, row),
                    Some(Action::Power(action)),
                    "clicking the first column of {glyph}"
                );
                let last = col + crate::util::text::width(glyph) as u16 - 1;
                assert_eq!(
                    hits.at(last, row),
                    Some(Action::Power(action)),
                    "clicking the last column of {glyph}"
                );
            }
        }
    }

    /// The transport buttons must be clickable **on the glyph itself**. The first
    /// version worked the columns out by hand and put every area one column to the
    /// right of what it drew.
    #[test]
    fn media_buttons_are_clickable_where_they_are_drawn() {
        use crate::sources::media::{Media, Status};
        for nerd_font in [true, false] {
            let mut state = AppState::new(Config::default());
            state.config.theme.nerd_font = nerd_font;
            let theme = Theme::from_config(&state.config.theme);
            state.media.latest = Some(Media {
                player: "firefox".into(),
                status: Status::Playing,
                position_us: 81_000_000,
                length_us: 265_000_000,
                title: "Sample Track".into(),
                artist: "Sample Artist".into(),
                album: "Sample Album".into(),
                art_url: String::new(),
            });
            let mut hits = HitMap::default();
            let mut terminal = Terminal::new(TestBackend::new(145, 18)).unwrap();
            terminal.draw(|f| draw(f, &state, &mut hits)).unwrap();
            let b = terminal.backend().buffer().clone();
            let text: Vec<String> = (0..b.area.height)
                .map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect())
                .collect();

            for (glyph, expected) in [
                (theme.icon_media_prev(), Action::Previous),
                // Playing, so the button offers the pause.
                (theme.icon_paused(), Action::PlayPause),
                (theme.icon_media_next(), Action::Next),
            ] {
                let (row, col) = text
                    .iter()
                    .enumerate()
                    .find_map(|(y, line)| {
                        line.find(glyph)
                            .map(|i| (y as u16, line[..i].chars().count() as u16))
                    })
                    .unwrap_or_else(|| panic!("{glyph} should be drawn (nerd {nerd_font})"));
                assert_eq!(
                    hits.at(col, row),
                    Some(expected),
                    "clicking {glyph} (nerd {nerd_font})"
                );
            }
        }
    }

    /// The timestamps grow past an hour. The buttons must not care - to the right of
    /// them they would move six columns away from their hit areas.
    #[test]
    fn an_hour_long_track_does_not_move_the_buttons() {
        use crate::sources::media::{Media, Status};
        let theme = Theme::default();
        let mut columns = Vec::new();
        for length_us in [265_000_000u64, 4_200_000_000] {
            let mut state = AppState::new(Config::default());
            state.media.latest = Some(Media {
                player: "firefox".into(),
                status: Status::Paused,
                position_us: length_us / 2,
                length_us,
                title: "Sample Track".into(),
                artist: String::new(),
                album: String::new(),
                art_url: String::new(),
            });
            let mut hits = HitMap::default();
            let mut terminal = Terminal::new(TestBackend::new(145, 18)).unwrap();
            terminal.draw(|f| draw(f, &state, &mut hits)).unwrap();
            let b = terminal.backend().buffer().clone();
            let text: Vec<String> = (0..b.area.height)
                .map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect())
                .collect();
            // Paused, so the button offers the play.
            let glyph = theme.icon_playing();
            let (row, col) = text
                .iter()
                .enumerate()
                .find_map(|(y, line)| {
                    line.find(glyph)
                        .map(|i| (y as u16, line[..i].chars().count() as u16))
                })
                .unwrap_or_else(|| panic!("{glyph} should be drawn"));
            assert_eq!(
                hits.at(col, row),
                Some(Action::PlayPause),
                "length {length_us}"
            );
            columns.push(col);
        }
        assert_eq!(
            columns[0], columns[1],
            "the button must not move with the clock"
        );
    }

    /// A stream with no length cannot be seeked, but it can still be paused.
    #[test]
    fn a_track_without_a_length_keeps_its_buttons() {
        use crate::sources::media::{Media, Status};
        let mut state = AppState::new(Config::default());
        state.media.latest = Some(Media {
            player: "firefox".into(),
            status: Status::Playing,
            position_us: 0,
            length_us: 0,
            title: "Live Stream".into(),
            artist: String::new(),
            album: String::new(),
            art_url: String::new(),
        });
        let mut hits = HitMap::default();
        let mut terminal = Terminal::new(TestBackend::new(145, 18)).unwrap();
        terminal.draw(|f| draw(f, &state, &mut hits)).unwrap();

        let found: Vec<Action> = (0..145u16)
            .flat_map(|x| (0..18u16).map(move |y| (x, y)))
            .filter_map(|(x, y)| hits.at(x, y))
            .collect();
        assert!(
            found.contains(&Action::PlayPause),
            "play/pause must stay clickable"
        );
        assert!(
            !found.iter().any(|a| matches!(a, Action::SeekBar { .. })),
            "there is nothing to seek within"
        );
    }

    /// While an action is pending, the power buttons must drop their hit areas.
    #[test]
    fn power_buttons_are_not_clickable_while_running() {
        use crate::sources::power::PowerAction;
        let mut state = AppState::new(Config::default());
        state.ui.power_running = Some(PowerAction::Suspend);
        let mut hits = HitMap::default();
        let mut terminal = Terminal::new(TestBackend::new(145, 18)).unwrap();
        terminal.draw(|f| draw(f, &state, &mut hits)).unwrap();
        let b = terminal.backend().buffer().clone();
        let text: Vec<String> = (0..b.area.height)
            .map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect())
            .collect();
        let glyph = Theme::default().icon_power_action(PowerAction::Suspend);
        let (row, col) = text
            .iter()
            .enumerate()
            .find_map(|(y, line)| {
                line.find(glyph)
                    .map(|i| (y as u16, line[..i].chars().count() as u16))
            })
            .expect("button should still be drawn");
        assert_eq!(
            hits.at(col, row),
            None,
            "must not be clickable while running"
        );
    }

    /// While the modal is up, clicks must not reach what is underneath.
    #[test]
    fn modal_covers_everything_underneath() {
        use crate::sources::power::PowerAction;
        let mut state = AppState::new(Config::default());
        state.ui.pending_power = Some(PowerAction::PowerOff);
        let mut hits = HitMap::default();
        let mut terminal = Terminal::new(TestBackend::new(145, 18)).unwrap();
        terminal.draw(|f| draw(f, &state, &mut hits)).unwrap();

        // The top-left, normally empty, counts as cancel.
        assert_eq!(hits.at(1, 1), Some(Action::ConfirmCancel));
        // The power row is covered by the modal too.
        assert_eq!(hits.at(3, 16), Some(Action::ConfirmCancel));
    }

    /// With no player, the row must survive and still say so.
    #[test]
    fn no_player_says_so() {
        let state = AppState::new(Config::default());
        let out = render(&state);
        assert!(out.contains("no media"), "{out}");
    }

    /// With nothing at all (just after boot, before any interface appears) it still says something.
    #[test]
    fn no_interfaces_says_so() {
        let state = AppState::new(Config::default());
        let out = render(&state);
        assert!(out.contains("no active interfaces"), "{out}");
    }

    /// With every source still empty (the first frame after startup) it must not panic, and the frame holds.
    #[test]
    fn first_frame_before_any_source_is_intact() {
        let state = AppState::new(Config::default());
        let out = render(&state);
        for (i, line) in out.lines().enumerate() {
            assert_eq!(line.chars().count(), 145, "line {i}");
        }
        assert_eq!(out.lines().count(), 18);
    }
}

#[cfg(test)]
mod confirm_tests {
    use super::*;
    use crate::config::Config;
    use crate::sources::power::PowerAction;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render_modal(action: PowerAction, yes: bool) -> String {
        let mut state = AppState::new(Config::default());
        state.ui.pending_power = Some(action);
        state.ui.confirm_yes_focused = yes;
        let mut hits = HitMap::default();
        let mut terminal = Terminal::new(TestBackend::new(145, 18)).unwrap();
        terminal.draw(|f| draw(f, &state, &mut hits)).unwrap();
        let b = terminal.backend().buffer().clone();
        (0..b.area.height)
            .map(|y| {
                (0..b.area.width)
                    .map(|x| b[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn modal_states_what_will_happen() {
        let out = render_modal(PowerAction::PowerOff, false);
        assert!(out.contains("Power off this machine?"), "{out}");
        assert!(out.contains("[ Yes ]"), "{out}");
        assert!(out.contains("[ No ]"), "{out}");
        assert!(out.contains("Esc / n: no"), "{out}");
    }

    #[test]
    fn each_action_has_its_own_question() {
        assert!(render_modal(PowerAction::Suspend, false).contains("Suspend this machine?"));
        assert!(render_modal(PowerAction::ScreenOff, false).contains("Turn the screen off?"));
    }

    /// Raising the modal must not break the frame.
    #[test]
    fn print_modal_for_review() {
        println!("{}", render_modal(PowerAction::PowerOff, false));
    }

    #[test]
    fn modal_keeps_the_frame_intact() {
        let out = render_modal(PowerAction::PowerOff, false);
        for (i, line) in out.lines().enumerate() {
            assert_eq!(line.chars().count(), 145, "line {i}");
        }
        assert_eq!(out.lines().count(), 18);
    }
}
