//! The currently playing track (mpris).

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::sources::media::Status;
use crate::store::MediaStore;
use crate::ui::hit::Action;
use crate::ui::slot::{DrawCtx, Measure, Rendered, Slot, priority};
use crate::ui::theme::{Part, Theme};
use crate::util::text::{fit, width};

/// Width of the seek bar, in columns.
pub const SEEK_WIDTH: usize = 26;
/// Columns of indent, and the gap after the buttons.
const INDENT: usize = 2;
const GAP: usize = 1;

const PLAYED: char = '━';
const HEAD: char = '╸';
const REMAIN: char = '─';

/// Progress bar. `ratio` is 0.0..=1.0.
pub fn seek_bar(ratio: f64, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let ratio = ratio.clamp(0.0, 1.0);
    let head = ((ratio * width as f64).round() as usize).min(width.saturating_sub(1));
    let mut s = String::with_capacity(width);
    for _ in 0..head {
        s.push(PLAYED);
    }
    s.push(HEAD);
    while s.chars().count() < width {
        s.push(REMAIN);
    }
    s
}

/// Format microseconds as `M:SS`, or `H:MM:SS` past an hour.
pub fn clock(us: u64) -> String {
    let total = us / 1_000_000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Columns one transport button takes.
///
/// The widest of the four glyphs wins, so the buttons stay evenly spaced and - more
/// to the point - **the seek bar does not shift a column when playback toggles**.
/// The ASCII fallbacks are `<<`, `||`, `>>` and `>`, which differ by a column.
fn button_width(theme: &Theme) -> usize {
    [
        theme.icon_media_prev(),
        theme.icon_playing(),
        theme.icon_paused(),
        theme.icon_media_next(),
    ]
    .iter()
    .map(|glyph| width(glyph))
    .max()
    .unwrap_or(1)
}

/// The play/pause button shows **what pressing it does**, not what is happening now:
/// while a track plays the button pauses it. `icon_status` is the other way round on
/// purpose - that one is a readout.
fn play_pause_glyph(status: Status, theme: &Theme) -> &'static str {
    if status == Status::Playing {
        theme.icon_paused()
    } else {
        theme.icon_playing()
    }
}

/// The transport buttons: their spans, and the columns each one occupies.
///
/// Both come out of the same walk. The previous version worked the hit areas out by
/// hand (`seek_x + SEEK_WIDTH + 18`, then a stride of 7), which put every area a
/// column to the right of its glyph and drifted six more once a track ran past an
/// hour and the timestamps grew.
fn buttons<'a>(status: Status, theme: &Theme) -> (Vec<Span<'a>>, Vec<(Action, u16, u16)>) {
    let cell = button_width(theme);
    let mut spans = Vec::new();
    let mut areas = Vec::new();
    let mut offset = 0usize;
    for (action, glyph) in [
        (Action::Previous, theme.icon_media_prev()),
        (Action::PlayPause, play_pause_glyph(status, theme)),
        (Action::Next, theme.icon_media_next()),
    ] {
        // Padded out to the cell so a narrow glyph still lines up with its neighbours.
        spans.push(Span::styled(
            format!("{glyph}{}", " ".repeat(cell.saturating_sub(width(glyph)))),
            Style::default().fg(theme.text),
        ));
        spans.push(Span::raw(" ".repeat(GAP)));
        // The gap belongs to the button on its left: a one-column glyph is a hard
        // target, and the areas still cannot overlap.
        areas.push((action, offset as u16, (cell + GAP) as u16));
        offset += cell + GAP;
    }
    (spans, areas)
}

/// Columns from the start of the pane to where the buttons begin.
fn buttons_x(area_x: u16) -> u16 {
    area_x + INDENT as u16
}

/// The column the seek bar starts in. Derived, not a constant: the button field is
/// wider without a Nerd Font.
pub fn seek_x(area_x: u16, theme: &Theme) -> u16 {
    buttons_x(area_x) + (3 * (button_width(theme) + GAP) + GAP) as u16
}

/// The metadata fields in the order they are shown, each with the mark that
/// introduces it. Empty ones are dropped, so a track with no album leaves no
/// dangling mark behind.
fn fields<'a>(
    media: &'a crate::sources::media::Media,
    theme: &Theme,
) -> Vec<(Option<&'static str>, &'a str)> {
    [
        (theme.icon_track(), media.title.as_str()),
        (theme.icon_artist(), media.artist.as_str()),
        (theme.icon_album(), media.album.as_str()),
    ]
    .into_iter()
    .filter(|(_, text)| !text.trim().is_empty())
    .collect()
}

/// Lay the metadata out in exactly `width` columns.
///
/// Each field is led by a mark saying which field it is, rather than a separator
/// saying only that a new one started. The mark takes the row's leading glyph slot;
/// a play/pause readout there would say the opposite of the button below it.
/// Without a Nerd Font there are no marks and ` / ` separates the fields.
///
/// Whatever runs past the budget is cut from the end, so the title survives and the
/// album is the first thing to go.
fn metadata<'a>(
    fields: &[(Option<&'static str>, &str)],
    width: usize,
    theme: &Theme,
) -> Vec<Span<'a>> {
    let mark = Style::default().fg(theme.dim);
    let body = Style::default().fg(theme.text);

    let mut pieces: Vec<(String, Style)> = Vec::new();
    if fields.is_empty() {
        pieces.push(("(no metadata)".to_string(), body));
    }
    for (i, (icon, text)) in fields.iter().enumerate() {
        match icon {
            // The mark doubles as the separator, so it only needs a gap before it.
            Some(glyph) => {
                if i > 0 {
                    pieces.push(("  ".to_string(), body));
                }
                pieces.push((format!("{glyph} "), mark));
            }
            None if i > 0 => pieces.push((" / ".to_string(), mark)),
            None => {}
        }
        pieces.push(((*text).to_string(), body));
    }

    let mut out = Vec::new();
    let mut used = 0;
    for (text, style) in pieces {
        if used >= width {
            break;
        }
        let columns = crate::util::text::width(&text);
        if used + columns <= width {
            used += columns;
            out.push(Span::styled(text, style));
        } else {
            // The piece that ran out is ellipsised; the ones after it are dropped.
            out.push(Span::styled(fit(&text, width - used), style));
            used = width;
        }
    }
    // Pad so the player's name keeps its column whatever the metadata is.
    if used < width {
        out.push(Span::raw(" ".repeat(width - used)));
    }
    out
}

/// The rows above the transport: one field to a row, or everything packed onto one row
/// when that is all the slot was given.
///
/// The fields are laid out top down and any spare rows fall between the last of them and
/// the transport, so what the track says changes the middle of the block rather than
/// where its controls are. A field past the rows on offer is dropped, the album first.
fn metadata_rows<'a>(
    media: &crate::sources::media::Media,
    player: &str,
    rows: usize,
    columns: u16,
    theme: &Theme,
) -> Vec<Line<'a>> {
    let fields = fields(media, theme);
    let dim = Style::default().fg(theme.dim);
    // Only the row carrying the player's name pays for it.
    let budget = |first: bool| {
        (columns as usize).saturating_sub(INDENT + INDENT + if first { width(player) } else { 0 })
    };

    if rows <= 1 {
        let mut spans = vec![Span::raw(" ".repeat(INDENT))];
        spans.extend(metadata(&fields, budget(true), theme));
        spans.push(Span::styled(player.to_string(), dim));
        return vec![Line::from(spans)];
    }

    (0..rows)
        .map(|i| {
            let mut spans = vec![Span::raw(" ".repeat(INDENT))];
            match fields.get(i) {
                Some(field) => {
                    spans.extend(metadata(std::slice::from_ref(field), budget(i == 0), theme))
                }
                // A player reporting nothing at all still gets a first row saying so.
                None if i == 0 => spans.extend(metadata(&[], budget(true), theme)),
                None => {}
            }
            if i == 0 {
                spans.push(Span::styled(player.to_string(), dim));
            }
            Line::from(spans)
        })
        .collect()
}

/// What the track block reads.
pub struct MediaSlot<'a> {
    pub media: &'a MediaStore,
}

impl Slot for MediaSlot<'_> {
    fn part(&self) -> Part {
        Part::Media
    }

    /// A row per metadata field with the transport under them, down to the two-row form
    /// - everything packed onto one row - a pane with less to spare gets.
    fn measure(&self, _width: u16) -> Measure {
        Measure {
            // The title and the transport. Below that there is no player worth drawing.
            min: 2,
            // Title, artist, album, a blank, and the transport.
            preferred: 5,
            priority: priority::MEDIA,
        }
    }

    fn render(&self, ctx: &DrawCtx, area: Rect) -> Rendered<'static> {
        let theme = ctx.theme;
        let mut hits: Vec<(Rect, Action)> = Vec::new();
        let Some(media) = &self.media.latest else {
            return Rendered::new(vec![Line::styled(
                format!("  {}  no media", theme.icon_note()),
                Style::default().fg(theme.dim),
            )]);
        };

        // The player's name is pinned to the right of the first row, so what is left is
        // that row's budget. Worked out from the strings rather than reserved as a
        // constant, so it follows whatever the row actually draws.
        // `INDENT` twice - the rows keep the same margin on the right as the divider.
        let player = format!("  {}", media.player);
        let rows = (area.height as usize).max(2);
        let lines = metadata_rows(media, &player, rows - 1, area.width, theme);

        let position = self.media.position_us();
        let ratio = if media.length_us > 0 {
            position as f64 / media.length_us as f64
        } else {
            0.0
        };

        // The transport is the last row the slot was given, whatever the metadata above
        // it came to: the buttons stay put when a track turns up with no album.
        let bar_row = area.y + rows as u16 - 1;
        let (button_spans, button_areas) = buttons(media.status, theme);
        for (action, offset, columns) in button_areas {
            // A stream with no length can still be paused or skipped, so these are
            // registered whatever the seek bar does.
            hits.push((
                Rect::new(buttons_x(area.x) + offset, bar_row, columns, 1),
                action,
            ));
        }

        let seek_x = seek_x(area.x, theme);
        if media.length_us > 0 {
            hits.push((
                Rect::new(seek_x, bar_row, SEEK_WIDTH as u16, 1),
                Action::SeekBar {
                    x: seek_x,
                    width: SEEK_WIDTH as u16,
                },
            ));
        }

        let mut transport = vec![Span::raw(" ".repeat(INDENT))];
        transport.extend(button_spans);
        transport.push(Span::raw(" ".repeat(GAP)));
        transport.push(Span::styled(
            seek_bar(ratio, SEEK_WIDTH),
            Style::default().fg(if media.status == Status::Playing {
                theme.accent
            } else {
                theme.dim
            }),
        ));
        transport.push(Span::styled(
            format!("  {} / {}", clock(position), clock(media.length_us)),
            Style::default().fg(theme.text),
        ));

        let mut lines = lines;
        lines.push(Line::from(transport));
        Rendered { lines, hits }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seek_bar_is_always_exactly_width() {
        for r in [0.0, 0.01, 0.5, 0.99, 1.0] {
            assert_eq!(
                seek_bar(r, SEEK_WIDTH).chars().count(),
                SEEK_WIDTH,
                "ratio {r}"
            );
        }
    }

    #[test]
    fn head_moves_with_progress() {
        let start = seek_bar(0.0, 10);
        let end = seek_bar(1.0, 10);
        assert!(start.starts_with(HEAD), "{start}");
        assert!(end.ends_with(REMAIN) || end.ends_with(HEAD), "{end}");
        assert!(
            start.chars().filter(|c| *c == PLAYED).count()
                < end.chars().filter(|c| *c == PLAYED).count()
        );
    }

    /// An out-of-range ratio must not break the column count.
    #[test]
    fn out_of_range_ratio_is_clamped() {
        assert_eq!(seek_bar(-1.0, 10).chars().count(), 10);
        assert_eq!(seek_bar(5.0, 10).chars().count(), 10);
    }

    #[test]
    fn zero_width_is_safe() {
        assert_eq!(seek_bar(0.5, 0), "");
    }

    #[test]
    fn clock_formats_minutes_and_hours() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(5_000_000), "0:05");
        assert_eq!(clock(65_000_000), "1:05");
        assert_eq!(clock(265_000_000), "4:25");
        assert_eq!(clock(3_600_000_000), "1:00:00");
        assert_eq!(clock(3_725_000_000), "1:02:05");
    }

    fn media(title: &str, artist: &str, album: &str) -> crate::sources::media::Media {
        crate::sources::media::Media {
            player: "firefox".into(),
            status: Status::Playing,
            position_us: 0,
            length_us: 1,
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            art_url: String::new(),
        }
    }

    fn laid_out(title: &str, artist: &str, album: &str, columns: usize, nerd_font: bool) -> String {
        let theme = Theme {
            nerd_font,
            ..Theme::default()
        };
        let track = media(title, artist, album);
        metadata(&fields(&track, &theme), columns, &theme)
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    /// The player's name sits to the right of it, so the metadata has to be exactly
    /// its budget however long the track is.
    #[test]
    fn the_metadata_is_always_exactly_its_budget() {
        for columns in [0, 1, 4, 20, 60] {
            for nerd_font in [true, false] {
                let out = laid_out("Track", "Artist", "Album", columns, nerd_font);
                assert_eq!(width(&out), columns, "{columns} columns, nerd={nerd_font}");
            }
        }
    }

    /// A mark in front of each field says *which* field it is. The slashes only said
    /// that a new one had started.
    #[test]
    fn each_field_is_introduced_by_its_own_mark() {
        let theme = Theme::default();
        let out = laid_out("Track", "Artist", "Album", 60, true);
        assert!(
            !out.contains(" / "),
            "the slashes are the fallback now: {out:?}"
        );
        for mark in [
            theme.icon_track().unwrap(),
            theme.icon_artist().unwrap(),
            theme.icon_album().unwrap(),
        ] {
            assert!(out.contains(mark), "{mark} missing from {out:?}");
        }
    }

    /// Without a Nerd Font there are no marks, so the slashes come back.
    #[test]
    fn the_ascii_fallback_goes_back_to_slashes() {
        let out = laid_out("Track", "Artist", "Album", 60, false);
        assert!(out.starts_with("Track / Artist / Album"), "{out:?}");
    }

    /// A track with no album must not leave the album's mark standing on its own.
    #[test]
    fn a_missing_field_takes_its_mark_with_it() {
        let theme = Theme::default();
        let out = laid_out("Track", "", "", 60, true);
        assert!(out.contains(theme.icon_track().unwrap()));
        assert!(!out.contains(theme.icon_artist().unwrap()), "{out:?}");
        assert!(!out.contains(theme.icon_album().unwrap()), "{out:?}");
        assert_eq!(laid_out("Track", "", "", 60, false).trim(), "Track");
    }

    /// The title is what you look at; the album is what you can lose.
    #[test]
    fn the_title_survives_a_budget_the_album_does_not() {
        let theme = Theme::default();
        let out = laid_out("Track", "Artist", "Album", 16, true);
        assert!(out.contains("Track"), "{out:?}");
        assert!(!out.contains("Album"), "{out:?}");
        assert!(!out.contains(theme.icon_album().unwrap()), "{out:?}");
    }

    /// A player that reports nothing at all still gets a row rather than a blank.
    #[test]
    fn a_track_with_no_metadata_says_so() {
        assert_eq!(laid_out("", "", "", 20, true).trim(), "(no metadata)");
        assert_eq!(laid_out(" ", "", "", 20, false).trim(), "(no metadata)");
    }

    /// The button shows the action, not the state: pressing it while a track plays
    /// pauses it, so it must read as a pause button.
    #[test]
    fn play_pause_button_shows_the_action_not_the_state() {
        let theme = Theme::default();
        assert_eq!(
            play_pause_glyph(Status::Playing, &theme),
            theme.icon_paused()
        );
        assert_eq!(
            play_pause_glyph(Status::Paused, &theme),
            theme.icon_playing()
        );
        assert_eq!(
            play_pause_glyph(Status::Stopped, &theme),
            theme.icon_playing()
        );
    }

    /// Toggling playback must not move the seek bar, whatever the glyph widths are.
    #[test]
    fn the_seek_bar_does_not_shift_when_playback_toggles() {
        for nerd_font in [true, false] {
            let theme = Theme {
                nerd_font,
                ..Theme::default()
            };
            let drawn = |status| {
                let (spans, _) = buttons(status, &theme);
                spans
                    .iter()
                    .map(|s| width(s.content.as_ref()))
                    .sum::<usize>()
            };
            assert_eq!(drawn(Status::Playing), drawn(Status::Paused), "{nerd_font}");
            // And the drawn field is exactly what `seek_x` accounts for.
            assert_eq!(
                seek_x(0, &theme) as usize,
                INDENT + drawn(Status::Playing) + GAP
            );
        }
    }

    /// The areas are laid out left to right and never overlap.
    #[test]
    fn button_areas_are_adjacent_and_ordered() {
        for nerd_font in [true, false] {
            let theme = Theme {
                nerd_font,
                ..Theme::default()
            };
            let (_, areas) = buttons(Status::Playing, &theme);
            assert_eq!(
                areas.iter().map(|(a, _, _)| *a).collect::<Vec<_>>(),
                vec![Action::Previous, Action::PlayPause, Action::Next]
            );
            for pair in areas.windows(2) {
                let (_, x, w) = pair[0];
                let (_, next, _) = pair[1];
                assert_eq!(x + w, next, "areas must abut without overlapping");
            }
        }
    }
}
