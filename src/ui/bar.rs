//! The bar: everything worth saying on one line.
//!
//! A terminal this short is being read, not clicked, so nothing here registers a hit
//! area - a confirmation modal would not fit on the rows a bar has.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::sources::media::Status;
use crate::state::AppState;
use crate::util::text::{truncate, width};

use super::slot::{Measure, allocate, priority};
use super::theme::{Part, Theme};
use super::workspaces::{Cell, CellState, cells};

/// What separates two segments.
const SEPARATOR: &str = " │ ";

/// Columns the track is allowed before it is cut.
const TRACK_WIDTH: usize = 28;

/// One thing the bar has to say, and how hard it holds its columns.
struct Segment {
    spans: Vec<Span<'static>>,
    priority: u8,
}

impl Segment {
    fn new(priority: u8, spans: Vec<Span<'static>>) -> Self {
        Self { spans, priority }
    }

    fn columns(&self) -> u16 {
        self.spans
            .iter()
            .map(|span| width(span.content.as_ref()))
            .sum::<usize>() as u16
    }
}

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let segments = segments(state, theme);

    // A separator costs columns too, so it is measured with the segment it follows.
    let separator = width(SEPARATOR) as u16;
    let measures: Vec<Measure> = segments
        .iter()
        .enumerate()
        .map(|(i, segment)| {
            let columns = segment.columns() + if i == 0 { 0 } else { separator };
            Measure {
                min: columns,
                preferred: columns,
                priority: segment.priority,
            }
        })
        .collect();
    let granted = allocate(&measures, area.width.saturating_sub(2));

    let mut spans = vec![Span::raw(" ")];
    for (segment, granted) in segments.into_iter().zip(&granted) {
        if *granted == 0 {
            continue;
        }
        if spans.len() > 1 {
            spans.push(Span::styled(SEPARATOR, Style::default().fg(theme.border)));
        }
        spans.extend(segment.spans);
    }

    // On two or three rows the line sits in the middle, away from the edges the
    // terminal's own chrome tends to crowd.
    let y = area.y + area.height / 2;
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect::new(area.x, y, area.width, 1),
    );
}

fn segments(state: &AppState, theme: &Theme) -> Vec<Segment> {
    let mut out = Vec::new();

    if let Some(spans) = workspace_spans(state, &theme.part(Part::Workspaces)) {
        out.push(Segment::new(priority::WORKSPACES, spans));
    }
    if let Some(spans) = media_spans(state, &theme.part(Part::Media)) {
        out.push(Segment::new(priority::MEDIA, spans));
    }
    out.push(Segment::new(
        priority::CLOCK,
        clock_spans(state, &theme.part(Part::Clock)),
    ));
    if let Some(spans) = volume_spans(state, &theme.part(Part::Audio)) {
        out.push(Segment::new(priority::AUDIO, spans));
    }
    if let Some(spans) = sensor_spans(state, &theme.part(Part::Sensors)) {
        out.push(Segment::new(priority::SENSORS, spans));
    }
    if let Some(spans) = network_spans(state, &theme.part(Part::Network)) {
        out.push(Segment::new(priority::NETWORK, spans));
    }
    out.push(Segment::new(
        priority::POWER,
        vec![Span::styled(
            theme.icon_power().to_string(),
            Style::default().fg(theme.part(Part::Power).dim),
        )],
    ));
    out
}

/// `[1] 2 3 4` - the numbers are back, since there is no room for the map.
fn workspace_spans(state: &AppState, theme: &Theme) -> Option<Vec<Span<'static>>> {
    let cells = cells(&state.workspaces.latest, state.config.workspaces.count);
    if cells.is_empty() {
        return None;
    }
    // No gap between the cells: each one is already padded to the width of a bracketed
    // id, so the row keeps its shape as the focus moves and costs a third less.
    let spans = cells
        .iter()
        .map(|cell| Span::styled(label(cell), style(cell.state, theme)))
        .collect();
    Some(spans)
}

/// The focused workspace is bracketed rather than only coloured: on a bar the eye has
/// no map to fall back on, and a colour alone is easy to miss in a row of numbers.
fn label(cell: &Cell) -> String {
    match cell.state {
        CellState::Focused => format!("[{}]", cell.id),
        _ => format!(" {} ", cell.id),
    }
}

fn style(state: CellState, theme: &Theme) -> Style {
    match state {
        CellState::Focused => Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
        CellState::ActiveElsewhere => Style::default().fg(theme.accent),
        CellState::Occupied => Style::default().fg(theme.text),
        CellState::Idle => Style::default().fg(theme.dim),
        CellState::Missing => Style::default().fg(theme.border),
    }
}

fn media_spans(state: &AppState, theme: &Theme) -> Option<Vec<Span<'static>>> {
    let media = state.media.latest.as_ref()?;
    let glyph = match media.status {
        Status::Playing => theme.icon_playing(),
        _ => theme.icon_paused(),
    };
    let track = match (media.title.as_str(), media.artist.as_str()) {
        ("", artist) => artist.to_string(),
        (title, "") => title.to_string(),
        (title, artist) => format!("{title} / {artist}"),
    };
    Some(vec![
        Span::styled(format!("{glyph} "), Style::default().fg(theme.dim)),
        Span::styled(
            truncate(&track, TRACK_WIDTH),
            Style::default().fg(theme.text),
        ),
    ])
}

fn clock_spans(state: &AppState, theme: &Theme) -> Vec<Span<'static>> {
    vec![Span::styled(
        state.now.format(&state.config.clock.format).to_string(),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )]
}

fn volume_spans(state: &AppState, theme: &Theme) -> Option<Vec<Span<'static>>> {
    let sink = state.audio.latest.sink.as_ref()?;
    let (glyph, colour) = if sink.muted {
        (theme.icon_volume_muted(), theme.warn)
    } else {
        (theme.icon_volume(), theme.text)
    };
    Some(vec![Span::styled(
        format!("{glyph} {}%", sink.volume),
        Style::default().fg(colour),
    )])
}

/// The two readings the panel would draw a trend for, which are the two worth watching.
fn sensor_spans(state: &AppState, theme: &Theme) -> Option<Vec<Span<'static>>> {
    let mut spans = Vec::new();
    let mut seen: Option<&str> = None;
    for reading in &state.sensors.latest {
        let first = seen != Some(reading.group.as_str());
        if !state
            .config
            .sensors
            .wants_sparkline(&reading.group, &reading.key, first)
        {
            continue;
        }
        seen = Some(&reading.group);
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!("{} {:.0}°C", reading.group, reading.celsius),
            Style::default().fg(theme.text),
        ));
    }
    (!spans.is_empty()).then_some(spans)
}

fn network_spans(state: &AppState, theme: &Theme) -> Option<Vec<Span<'static>>> {
    let iface = state.network.latest.interfaces.first()?;
    let glyph = theme.icon_net(iface.kind);
    let detail = match iface.wireless.as_ref().and_then(|w| w.rssi) {
        Some(rssi) => format!("{rssi} dBm"),
        None => iface.name.clone(),
    };
    Some(vec![Span::styled(
        format!("{glyph} {detail}"),
        Style::default().fg(theme.text),
    )])
}
