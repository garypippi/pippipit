//! The output and input rows.
//!
//! Both are drawn the same way - icon, bar, device name - because they are the same
//! kind of thing. The asymmetry the first version had (a bar for output, a bare name
//! for input) made the input look like a readout rather than a control.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::sources::audio::{Device, Target};
use crate::store::AudioStore;
use crate::ui::hit::Action;
use crate::ui::slot::{DrawCtx, Measure, Rendered, Slot, priority};
use crate::ui::theme::{Part, Theme};
use crate::util::text::{truncate, width};

/// Width of the volume bar, in columns.
pub const BAR_WIDTH: usize = 26;

/// Columns allowed for the device name.
const DEVICE_WIDTH: usize = 34;

/// Columns of indent, and the gap either side of the bar.
const INDENT: usize = 2;
const GAP: usize = 2;

const FILLED: char = '█';
const PARTIAL: char = '▍';
const EMPTY: char = '░';

/// The bar as characters, before the reading is laid over it.
///
/// 100% is full and anything above stays full (PipeWire goes up to 150%).
fn glyphs(volume: u16, width: usize) -> Vec<char> {
    if width == 0 {
        return Vec::new();
    }
    let ratio = (volume.min(100) as f32) / 100.0;
    let exact = ratio * width as f32;
    let full = exact.floor() as usize;
    let has_partial = full < width && (exact - full as f32) >= 0.5;

    let mut out: Vec<char> = Vec::with_capacity(width);
    out.extend(std::iter::repeat_n(FILLED, full.min(width)));
    if has_partial {
        out.push(PARTIAL);
    }
    out.extend(std::iter::repeat_n(EMPTY, width - out.len()));
    out
}

/// The bar with the reading centred **inside** it.
///
/// Left of the bar the number would cost six columns and read as a separate field.
/// Over the bar it costs nothing, but the boundary between the
/// filled and empty parts can fall in the middle of the text - so the style is
/// decided per cell and equal runs are merged into spans afterwards, the same way
/// the workspace mini-map colours its rows.
pub fn bar_spans<'a>(volume: u16, muted: bool, columns: usize, theme: &Theme) -> Vec<Span<'a>> {
    let mut cells = glyphs(volume, columns);
    if cells.is_empty() {
        return Vec::new();
    }
    // Padded, so the reading does not run straight into the blocks either side of it.
    // Over the filled part the padding is knocked out with the rest and reads as more
    // fill; over the empty part it reads as breathing room.
    let label: Vec<char> = format!(" {volume}% ").chars().collect();
    // Where the filled part ends, so a cell can tell which side of it the text is on.
    let filled = cells
        .iter()
        .take_while(|c| **c == FILLED || **c == PARTIAL)
        .count();

    let start = columns.saturating_sub(label.len()) / 2;
    let mut over_fill = vec![false; columns];
    for (i, ch) in label.iter().enumerate() {
        let Some(cell) = cells.get_mut(start + i) else {
            break;
        };
        *cell = *ch;
        over_fill[start + i] = start + i < filled;
    }

    // Muted says so by going quiet: the bar dims and the icon beside it changes.
    let colour = if muted { theme.dim } else { theme.accent };
    let plain = Style::default().fg(colour);
    // Text on top of the filled part is knocked out of it rather than drawn over it,
    // which needs no background colour of its own.
    let knockout = plain.add_modifier(Modifier::REVERSED);

    let mut spans: Vec<Span> = Vec::new();
    let mut run = String::new();
    let mut run_style = None;
    for (i, ch) in cells.into_iter().enumerate() {
        let style = if over_fill[i] { knockout } else { plain };
        if run_style != Some(style) {
            if let Some(previous) = run_style {
                spans.push(Span::styled(std::mem::take(&mut run), previous));
            }
            run_style = Some(style);
        }
        run.push(ch);
    }
    if let Some(style) = run_style {
        spans.push(Span::styled(run, style));
    }
    spans
}

fn icon(target: Target, muted: bool, theme: &Theme) -> &'static str {
    match (target, muted) {
        (Target::Sink, false) => theme.icon_volume(),
        (Target::Sink, true) => theme.icon_volume_muted(),
        (Target::Source, false) => theme.icon_mic(),
        (Target::Source, true) => theme.icon_mic_muted(),
    }
}

/// Columns the icon field takes.
///
/// The widest of the four wins, so output and input line up whatever the glyphs are:
/// the ASCII fallbacks are `Out:` and `In:`, which differ by a column.
pub fn icon_width(theme: &Theme) -> usize {
    [
        theme.icon_volume(),
        theme.icon_volume_muted(),
        theme.icon_mic(),
        theme.icon_mic_muted(),
    ]
    .iter()
    .map(|glyph| width(glyph))
    .max()
    .unwrap_or(1)
}

/// The column the bar starts in, given the pane it is drawn in.
pub fn bar_x(area_x: u16, theme: &Theme) -> u16 {
    area_x + (INDENT + icon_width(theme) + GAP) as u16
}

/// Device names are long (`GPU HDMI/DP Audio Digital Stereo (HDMI)` and the like).
/// Truncate by display width.
fn device_label(device: Option<&Device>, max: usize) -> String {
    truncate(device.map(|d| d.description.as_str()).unwrap_or("-"), max)
}

/// One row: the icon, the bar, and the device, with the two buttons on it.
fn row<'a>(
    target: Target,
    device: Option<&Device>,
    theme: &Theme,
    area: Rect,
    y: u16,
    hits: &mut Vec<(Rect, Action)>,
) -> Line<'a> {
    let volume = device.map(|d| d.volume).unwrap_or(0);
    let muted = device.map(|d| d.muted).unwrap_or(false);
    let glyph = icon(target, muted, theme);
    let icon_columns = icon_width(theme);

    let mut spans = vec![
        Span::raw(" ".repeat(INDENT)),
        Span::styled(
            format!(
                "{glyph}{}",
                " ".repeat(icon_columns.saturating_sub(width(glyph)))
            ),
            Style::default().fg(if muted { theme.warn } else { theme.accent }),
        ),
        Span::raw(" ".repeat(GAP)),
    ];
    spans.extend(bar_spans(volume, muted, BAR_WIDTH, theme));
    spans.push(Span::raw(" ".repeat(GAP)));
    // The name gets what the row has left, keeping the pane's margin on the right too:
    // with the album art beside it the row is that many columns short.
    let used = INDENT + icon_columns + GAP + BAR_WIDTH + GAP;
    let room = (area.width as usize).saturating_sub(used + INDENT);
    spans.push(Span::styled(
        device_label(device, DEVICE_WIDTH.min(room)),
        Style::default().fg(if muted { theme.dim } else { theme.text }),
    ));

    // The icon is the mute button, and the bar is the volume.
    hits.push((
        Rect::new(area.x + INDENT as u16, y, icon_columns as u16, 1),
        Action::ToggleMute { target },
    ));
    let x = bar_x(area.x, theme);
    hits.push((
        Rect::new(x, y, BAR_WIDTH as u16, 1),
        Action::VolumeBar {
            target,
            x,
            width: BAR_WIDTH as u16,
        },
    ));
    Line::from(spans)
}

/// What the volume block reads.
pub struct AudioSlot<'a> {
    pub audio: &'a AudioStore,
}

impl Slot for AudioSlot<'_> {
    fn part(&self) -> Part {
        Part::Audio
    }

    /// Output and input, one row each, whether or not a device answered.
    fn measure(&self, _width: u16) -> Measure {
        Measure::fixed(2, priority::AUDIO)
    }

    fn render(&self, ctx: &DrawCtx, area: Rect) -> Rendered<'static> {
        let theme = ctx.theme;
        let mut hits = Vec::new();
        let mut lines = vec![row(
            Target::Sink,
            self.audio.latest.sink.as_ref(),
            theme,
            area,
            area.y,
            &mut hits,
        )];

        if let Some(err) = &self.audio.error {
            // The reason takes the input row: there is no third row to put it on, and a
            // control that cannot be trusted to have read its device is worse than absent.
            lines.push(Line::styled(
                format!("  audio unavailable: {err}"),
                Style::default().fg(theme.dim),
            ));
            return Rendered { lines, hits };
        }

        lines.push(row(
            Target::Source,
            self.audio.latest.source.as_ref(),
            theme,
            area,
            area.y + 1,
            &mut hits,
        ));
        Rendered { lines, hits }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::hit::HitMap;

    /// The bar's glyphs as a string, which is all the shape tests need.
    fn bar(volume: u16, width: usize) -> String {
        glyphs(volume, width).into_iter().collect()
    }

    /// The text of a row, with the styles dropped.
    fn text(spans: &Line<'_>) -> String {
        spans.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn rendered(volume: u16, muted: bool) -> String {
        bar_spans(volume, muted, BAR_WIDTH, &Theme::default())
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn bar_is_always_exactly_width() {
        for v in [0u16, 1, 35, 50, 99, 100, 150] {
            assert_eq!(bar(v, BAR_WIDTH).chars().count(), BAR_WIDTH, "volume {v}");
        }
    }

    #[test]
    fn zero_is_empty_and_hundred_is_full() {
        assert_eq!(bar(0, 8), "░░░░░░░░");
        assert_eq!(bar(100, 8), "████████");
    }

    /// PipeWire can exceed 100%; the bar saturates.
    #[test]
    fn above_hundred_saturates() {
        assert_eq!(bar(150, 8), bar(100, 8));
    }

    #[test]
    fn partial_block_appears_mid_step() {
        // At 8 columns, 50% is exactly 4 cells with no remainder.
        assert_eq!(bar(50, 8), "████░░░░");
        // At 8 columns, 56% is 4.48 cells; the 0.48 remainder is dropped.
        assert_eq!(bar(56, 8), "████░░░░");
        // At 8 columns, 57% is 4.56 cells; the remainder passes half, so ▍ appears.
        assert_eq!(bar(57, 8), "████▍░░░");
    }

    #[test]
    fn zero_width_does_not_panic() {
        assert_eq!(bar(50, 0), "");
        assert!(bar_spans(50, false, 0, &Theme::default()).is_empty());
    }

    /// The reading sits in the bar, and the bar keeps its width whatever it says.
    #[test]
    fn the_reading_is_centred_inside_the_bar() {
        for volume in [0u16, 7, 35, 100, 150] {
            let row = rendered(volume, false);
            assert_eq!(width(&row), BAR_WIDTH, "volume {volume}");
            assert!(row.contains(&format!("{volume}%")), "{row:?}");
        }
    }

    /// The text has to be legible on both sides of the boundary, which means the run
    /// that overlaps the filled part is knocked out of it and the rest is not.
    #[test]
    fn the_reading_is_knocked_out_of_the_filled_part_only() {
        let theme = Theme::default();
        // At 50% of 26 columns the fill ends at column 13, and ` 50% ` starts at 10 -
        // so the text straddles the boundary.
        let spans = bar_spans(50, false, BAR_WIDTH, &theme);
        let knocked: String = spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(knocked, " 50", "the part over the fill, padding included");
        let plain: String = spans
            .iter()
            .filter(|s| !s.style.add_modifier.contains(Modifier::REVERSED))
            .map(|s| s.content.as_ref())
            .collect();
        assert!(plain.contains('%'), "{plain:?} keeps the rest as it is");
    }

    /// A reading entirely on the empty side is never knocked out.
    #[test]
    fn a_quiet_bar_keeps_the_reading_plain() {
        let spans = bar_spans(0, false, BAR_WIDTH, &Theme::default());
        assert!(
            spans
                .iter()
                .all(|s| !s.style.add_modifier.contains(Modifier::REVERSED))
        );
    }

    /// Mute is said by the colour of the bar and by the icon, not by a badge.
    #[test]
    fn muting_dims_the_bar_and_swaps_the_icon() {
        let theme = Theme::default();
        let muted = bar_spans(35, true, BAR_WIDTH, &theme);
        assert!(muted.iter().all(|s| s.style.fg == Some(theme.dim)));
        assert_ne!(
            icon(Target::Sink, true, &theme),
            icon(Target::Sink, false, &theme)
        );
        assert_ne!(
            icon(Target::Source, true, &theme),
            icon(Target::Source, false, &theme)
        );
    }

    fn store_with_devices() -> AudioStore {
        let mut store = AudioStore::default();
        store.latest.sink = Some(Device {
            description: "Speakers".into(),
            volume: 35,
            muted: false,
        });
        store.latest.source = Some(Device {
            description: "Microphone".into(),
            volume: 62,
            muted: false,
        });
        store
    }

    /// Draw the block, with the hit areas collected the way a pane collects them.
    fn render(store: &AudioStore, theme: &Theme, area: Rect) -> (Vec<Line<'static>>, HitMap) {
        let out = AudioSlot { audio: store }.render(&DrawCtx { theme }, area);
        let mut hits = HitMap::default();
        for (rect, action) in out.hits {
            hits.push(rect, action);
        }
        (out.lines, hits)
    }

    /// Output and input are the same shape, so the bars have to start in the same column.
    #[test]
    fn both_rows_line_up() {
        let theme = Theme::default();
        let (rows, hits) = render(&store_with_devices(), &theme, Rect::new(0, 10, 72, 2));
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.width() <= 72), "must fit the pane");
        let x = bar_x(0, &theme);
        for (i, row) in rows.iter().enumerate() {
            let text = text(row);
            let bar_at = text.chars().take(x as usize).collect::<String>();
            assert_eq!(width(&bar_at), x as usize, "row {i} indent");
        }
        // And each row's own bar and icon are clickable, on that row only.
        for (i, target) in [Target::Sink, Target::Source].into_iter().enumerate() {
            let y = 10 + i as u16;
            assert_eq!(
                hits.at(INDENT as u16, y),
                Some(Action::ToggleMute { target })
            );
            assert_eq!(
                hits.at(x, y),
                Some(Action::VolumeBar {
                    target,
                    x,
                    width: BAR_WIDTH as u16
                })
            );
            assert_eq!(
                hits.at(x + BAR_WIDTH as u16, y),
                None,
                "one past the bar is not the bar"
            );
        }
    }

    /// The ASCII fallbacks are `Out:` and `In:`, which differ by a column - the icon
    /// field is padded to the widest, or the two bars would start one apart.
    #[test]
    fn the_ascii_fallback_lines_up_too() {
        let theme = Theme {
            nerd_font: false,
            ..Theme::default()
        };
        let (rows, _hits) = render(&store_with_devices(), &theme, Rect::new(0, 0, 72, 2));
        let bars: Vec<usize> = rows
            .iter()
            .map(|row| {
                text(row)
                    .chars()
                    .position(|c| [FILLED, PARTIAL, EMPTY].contains(&c))
                    .expect("a bar on every row")
            })
            .collect();
        assert_eq!(bars[0], bars[1], "the bars must start in the same column");
        assert_eq!(bars[0], bar_x(0, &theme) as usize);
    }

    /// A failed read replaces the input row, and must not leave a control there that
    /// would act on a device it could not read.
    #[test]
    fn an_audio_error_takes_the_input_row() {
        let mut store = store_with_devices();
        store.error = Some("pactl gone".into());
        let (rows, hits) = render(&store, &Theme::default(), Rect::new(0, 10, 72, 2));
        assert!(text(&rows[1]).contains("audio unavailable"));
        assert_eq!(hits.at(INDENT as u16, 11), None);
    }
}
