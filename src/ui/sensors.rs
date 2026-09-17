//! The temperature panel: a trend row for the readings that asked for one, and a
//! two-column grid for the rest.
//!
//! How many sensors exist depends on the system and its kernel config. The count is
//! never assumed; whatever is found flows into the columns.
//!
//! A reading named in `[sensors] sparkline` gets a row of its own, and the trend takes
//! what is left of it. Beside the value it would get eight columns, forty seconds of a
//! five minute history - too narrow to read a shape in.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::config::SensorsConfig;
use crate::sources::sensors::Reading;
use ratatui::layout::Rect;

use crate::store::SensorStore;
use crate::ui::slot::{DrawCtx, Measure, Rendered, Slot, priority};
use crate::ui::theme::{Part, Theme};
use crate::util::sparkline::Scale;

/// The trend is read as a shape, so the scale is linear and the floor is five degrees:
/// without it a half-degree wobble would swing across the full height.
const TREND: Scale = Scale::Linear { min_range: 5.0 };

/// The pane's left margin, shared with everything else on screen.
const INDENT: usize = 2;
/// Columns the group heading gets.
const GROUP_WIDTH: usize = 5;
/// Columns the sensor label gets. `Motherboard` is eleven.
const LABEL_WIDTH: usize = 11;
/// Columns the reading gets, right-aligned. `100°C` is five.
const VALUE_WIDTH: usize = 5;

/// Below this fraction of `warn`, a sensor is cool enough not to be worth looking at.
///
/// The thresholds were only ever used as a switch, so 35°C and 74°C drew identically
/// and nothing said how much headroom was left until the moment it ran out. This is
/// the cheapest thing that fixes it: one more step, no extra columns, and the palette
/// stays the four named colours (interpolating between them would need RGB).
const COOL: f32 = 0.6;

/// How much of the panel a trend row spends before the sparkline starts.
fn trend_prefix() -> usize {
    INDENT + GROUP_WIDTH + LABEL_WIDTH + VALUE_WIDTH + 2
}

/// The sparkline's width on a trend row, given the panel.
///
/// Derived rather than fixed, the same way the network one is: a narrower pane has to
/// shrink, and there is no point asking for more samples than the ring keeps.
fn trend_width(width: u16, history_len: usize) -> usize {
    (width as usize)
        .saturating_sub(trend_prefix() + INDENT)
        .min(history_len)
}

/// Which readings get a row of their own.
///
/// `first_in_group` is worked out over the **whole** list, because that is what the
/// `sparkline` setting means by a group name - the row that carries the heading.
fn wants_trend(config: &SensorsConfig, readings: &[Reading]) -> Vec<bool> {
    let mut prev: Option<&str> = None;
    readings
        .iter()
        .map(|r| {
            let first = prev != Some(r.group.as_str());
            prev = Some(&r.group);
            config.wants_sparkline(&r.group, &r.key, first)
        })
        .collect()
}

/// The colour of a reading, by how close it is to its own limit.
fn value_color(reading: &Reading, theme: &Theme) -> ratatui::style::Color {
    if reading.is_crit() {
        theme.crit
    } else if reading.is_warn() {
        theme.warn
    } else if reading.celsius < reading.warn * COOL {
        // Cool enough to skip over. Many sensors have no threshold in sysfs (six of ten
        // on the reference machine) and fall back to the configured 75/90, so this step
        // is as much of a guess as the switch it extends - but no more of one.
        theme.dim
    } else {
        theme.text
    }
}

/// One trend row: the reading, then its history across the rest of the panel.
fn trend_row<'a>(
    view: &SensorSlot,
    theme: &Theme,
    reading: &Reading,
    heading: bool,
    width: u16,
) -> Line<'a> {
    let spark = trend_width(width, view.history_len);
    let drawn = view
        .sensors
        .history
        .get(&reading.key)
        .map(|r| r.render(spark, TREND))
        .unwrap_or_default();
    Line::from(vec![
        Span::styled(
            format!(
                "{}{:<GROUP_WIDTH$}",
                " ".repeat(INDENT),
                if heading { reading.group.as_str() } else { "" }
            ),
            Style::default().fg(theme.dim),
        ),
        Span::styled(
            format!("{:<LABEL_WIDTH$}", reading.label),
            Style::default().fg(theme.text),
        ),
        Span::styled(
            format!("{:>VALUE_WIDTH$}", format!("{:.0}°C", reading.celsius)),
            Style::default().fg(value_color(reading, theme)),
        ),
        // Padding a short series out to the field is the caller's job: `render` gives
        // one character per sample, so the columns would shift as the ring filled.
        Span::styled(
            format!("  {drawn:<spark$}"),
            Style::default().fg(theme.sparkline),
        ),
    ])
}

/// Distribute into columns without splitting a group.
///
/// An even split by count would push the three `GPU` rows across the column boundary,
/// which breaks the heading elision (blank for every row after the first in a group).
fn split_columns(readings: &[&Reading], columns: usize) -> Vec<usize> {
    if columns <= 1 || readings.is_empty() {
        return vec![readings.len()];
    }
    let target = readings.len().div_ceil(columns);
    let mut sizes = Vec::new();
    let mut taken = 0usize;

    while taken < readings.len() {
        let remaining_cols = columns - sizes.len();
        if remaining_cols <= 1 {
            sizes.push(readings.len() - taken);
            break;
        }
        let mut end = (taken + target).min(readings.len());
        // Never cut inside a group: walk to the end of the one the boundary lands in.
        while end < readings.len() && readings[end].group == readings[end - 1].group {
            end += 1;
        }
        sizes.push(end - taken);
        taken = end;
    }
    while sizes.len() < columns {
        sizes.push(0);
    }
    sizes
}

/// One cell of the grid, as spans.
///
/// Three of them, not one: the heading and the label name the thing and the reading is
/// the thing. Drawn as a single span they shared a colour, so a sensor going over its
/// limit turned its own name yellow along with the number.
fn cell_spans<'a>(theme: &Theme, reading: Option<(&Reading, bool)>, width: usize) -> Vec<Span<'a>> {
    let Some((reading, heading)) = reading else {
        return vec![Span::raw(" ".repeat(width))];
    };
    let group = format!(
        "{}{:<GROUP_WIDTH$}",
        " ".repeat(INDENT),
        if heading { reading.group.as_str() } else { "" }
    );
    let label = format!("{:<LABEL_WIDTH$}", reading.label);
    let value = format!("{:>VALUE_WIDTH$}", format!("{:.0}°C", reading.celsius));
    let used = group.chars().count() + label.chars().count() + value.chars().count();
    if used > width {
        // Too narrow for the pair. The reading is the part that matters, so it is what
        // survives - and the cell is still exactly its column, or the grid stops
        // lining up on the row below.
        return vec![Span::styled(
            crate::util::text::fit(value.trim(), width),
            Style::default().fg(value_color(reading, theme)),
        )];
    }
    vec![
        Span::styled(group, Style::default().fg(theme.dim)),
        Span::styled(label, Style::default().fg(theme.text)),
        Span::styled(value, Style::default().fg(value_color(reading, theme))),
        Span::raw(" ".repeat(width - used)),
    ]
}

/// Return the temperature panel as a sequence of rows; the caller stacks it with
/// everything else.
/// What the temperature panel reads.
pub struct SensorSlot<'a> {
    pub sensors: &'a SensorStore,
    pub config: &'a SensorsConfig,
    pub history_len: usize,
    /// How many columns the grid is laid out in. A narrow layout passes 1; the config
    /// is what the two panes use.
    pub columns: usize,
}

impl Slot for SensorSlot<'_> {
    fn part(&self) -> Part {
        Part::Sensors
    }

    /// Two trend rows and a four-row grid at the reference size, and whatever the
    /// readings need below that.
    fn measure(&self, width: u16) -> Measure {
        let rows = self.lines(&Theme::default(), width).len() as u16;
        Measure {
            min: 1,
            preferred: rows,
            priority: priority::SENSORS,
        }
    }

    fn render(&self, ctx: &DrawCtx, area: Rect) -> Rendered<'static> {
        Rendered::new(self.lines(ctx.theme, area.width))
    }
}

impl SensorSlot<'_> {
    fn lines<'a>(&self, theme: &Theme, width: u16) -> Vec<Line<'a>> {
        if let Some(err) = &self.sensors.error
            && self.sensors.latest.is_empty()
        {
            return vec![Line::styled(
                format!("  sensors unavailable: {err}"),
                Style::default().fg(theme.dim),
            )];
        }
        if self.sensors.latest.is_empty() {
            return vec![Line::styled(
                "  no temperature sensors found",
                Style::default().fg(theme.dim),
            )];
        }

        let config: &SensorsConfig = self.config;
        let trended = wants_trend(config, &self.sensors.latest);

        let mut out = Vec::new();
        let mut prev: Option<&str> = None;
        for (reading, trend) in self.sensors.latest.iter().zip(&trended) {
            if !trend {
                continue;
            }
            let heading = prev != Some(reading.group.as_str());
            prev = Some(&reading.group);
            out.push(trend_row(self, theme, reading, heading, width));
        }

        // Everything else goes in the grid, and the heading elision is worked out again
        // over what is left: with `GPU Edge` promoted to a row, `Hotspot` is the first
        // `GPU` the grid sees and has to carry the name.
        let rest: Vec<&Reading> = self
            .sensors
            .latest
            .iter()
            .zip(&trended)
            .filter(|(_, trend)| !**trend)
            .map(|(reading, _)| reading)
            .collect();
        if rest.is_empty() {
            return out;
        }

        let sizes = split_columns(&rest, self.columns.max(1));
        let mut columns: Vec<&[&Reading]> = Vec::new();
        let mut offset = 0usize;
        for size in &sizes {
            columns.push(&rest[offset..offset + size]);
            offset += size;
        }

        // Which rows carry their group's name, over the grid's own ordering.
        let mut heading = Vec::with_capacity(rest.len());
        let mut prev: Option<&str> = None;
        for column in &columns {
            for reading in column.iter() {
                heading.push(prev != Some(reading.group.as_str()));
                prev = Some(&reading.group);
            }
        }

        let rows = columns.iter().map(|c| c.len()).max().unwrap_or(0);
        let col_width = (width as usize / columns.len().max(1)).max(20);
        let mut base = 0usize;
        let offsets: Vec<usize> = columns
            .iter()
            .map(|c| {
                let at = base;
                base += c.len();
                at
            })
            .collect();

        for row in 0..rows {
            let mut spans: Vec<Span<'a>> = Vec::new();
            for (column, start) in columns.iter().zip(&offsets) {
                let cell = column
                    .get(row)
                    .map(|reading| (*reading, heading[start + row]));
                spans.extend(cell_spans(theme, cell, col_width));
            }
            out.push(Line::from(spans));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::sparkline::Ring;

    fn reading(group: &str, label: &str) -> Reading {
        Reading {
            key: format!("{group}/{label}"),
            group: group.to_string(),
            label: label.to_string(),
            celsius: 42.0,
            warn: 75.0,
            crit: 90.0,
        }
    }

    /// Nine sensors, in the order the source reports them.
    fn nine() -> Vec<Reading> {
        vec![
            reading("CPU", "Package"),
            reading("GPU", "Edge"),
            reading("GPU", "Hotspot"),
            reading("GPU", "VRAM"),
            reading("WiFi", "Adapter"),
            reading("NVMe", "Drive"),
            reading("MB", "CPU"),
            reading("MB", "Motherboard"),
            reading("MB", "VRM"),
        ]
    }

    fn refs(readings: &[Reading]) -> Vec<&Reading> {
        readings.iter().collect()
    }

    fn store_with(readings: Vec<Reading>) -> SensorStore {
        let mut store = SensorStore::default();
        for r in &readings {
            let ring = store
                .history
                .entry(r.key.clone())
                .or_insert_with(|| Ring::new(60));
            for _ in 0..60 {
                ring.push(r.celsius);
            }
        }
        store.latest = readings;
        store
    }

    fn slot<'a>(store: &'a SensorStore, config: &'a SensorsConfig) -> SensorSlot<'a> {
        SensorSlot {
            sensors: store,
            config,
            history_len: 60,
            columns: config.columns,
        }
    }

    fn text(store: &SensorStore, config: &SensorsConfig, width: u16) -> Vec<String> {
        slot(store, config)
            .lines(&Theme::default(), width)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    /// The nine-sensor case. The three GPU rows must not straddle the columns.
    #[test]
    fn does_not_split_a_group_across_columns() {
        let readings = nine();
        let sizes = split_columns(&refs(&readings), 2);
        assert_eq!(sizes, vec![5, 4]);

        // The boundary lands exactly on a group break.
        assert_eq!(readings[4].group, "WiFi");
        assert_eq!(readings[5].group, "NVMe");
    }

    #[test]
    fn single_column_keeps_everything() {
        let readings = vec![reading("CPU", "a"), reading("GPU", "b")];
        assert_eq!(split_columns(&refs(&readings), 1), vec![2]);
    }

    #[test]
    fn handles_empty() {
        assert_eq!(split_columns(&[], 2), vec![0]);
    }

    #[test]
    fn five_sensors_split_evenly() {
        let readings = vec![
            reading("CPU", "a"),
            reading("GPU", "b"),
            reading("MB", "c"),
            reading("NVMe", "d"),
            reading("WiFi", "e"),
        ];
        assert_eq!(split_columns(&refs(&readings), 2), vec![3, 2]);
    }

    /// The point of the change: the trend was eight columns, which is forty seconds of
    /// a five minute history at the default tick. On its own row it gets the panel.
    #[test]
    fn a_trend_row_gives_the_sparkline_the_rest_of_the_panel() {
        assert_eq!(trend_width(70, 60), 43);
        // Never more samples than the ring keeps.
        assert_eq!(trend_width(70, 20), 20);
        // A narrow panel shrinks instead of overflowing.
        assert_eq!(trend_width(30, 60), 3);
        assert_eq!(trend_width(20, 60), 0);
    }

    /// Only the readings `[sensors] sparkline` names get a row; the rest go in the
    /// grid. The default names the two group headings, `CPU` and `GPU`.
    #[test]
    fn only_the_named_readings_get_a_row_of_their_own() {
        let rows = text(&store_with(nine()), &SensorsConfig::default(), 70);
        assert!(rows[0].starts_with("  CPU  Package"), "{:?}", rows[0]);
        assert!(rows[1].starts_with("  GPU  Edge"), "{:?}", rows[1]);
        for row in &rows[..2] {
            assert!(row.contains('▄') || row.contains('▅'), "a trend: {row:?}");
        }
        // Two trend rows and four grid rows: seven readings over two columns.
        assert_eq!(rows.len(), 6, "{rows:#?}");
    }

    /// With `GPU Edge` promoted to a row, `Hotspot` is the first `GPU` the grid sees
    /// and has to carry the heading - otherwise the group loses its name entirely.
    #[test]
    fn the_grid_works_out_its_own_headings() {
        let rows = text(&store_with(nine()), &SensorsConfig::default(), 70);
        assert!(rows[2].starts_with("  GPU  Hotspot"), "{:?}", rows[2]);
        assert!(rows[3].starts_with("       VRAM"), "{:?}", rows[3]);
    }

    /// A group nobody asked for gets no row, and asking for none leaves only the grid.
    #[test]
    fn a_panel_can_have_no_trends_at_all() {
        let config = SensorsConfig {
            sparkline: Vec::new(),
            ..SensorsConfig::default()
        };
        let rows = text(&store_with(nine()), &config, 70);
        assert_eq!(rows.len(), 5, "nine readings over two columns: {rows:#?}");
        assert!(rows[0].starts_with("  CPU  Package"), "{:?}", rows[0]);
        assert!(!rows[0].contains('▄'), "no trend anywhere: {:?}", rows[0]);
    }

    /// The heading and the label name the thing; the reading is the thing. Drawn as one
    /// span they shared a colour, so a sensor over its limit turned its own name yellow.
    #[test]
    fn only_the_reading_carries_the_heat() {
        let theme = Theme::default();
        let mut hot = reading("GPU", "Hotspot");
        hot.celsius = 80.0;
        let spans = cell_spans(&theme, Some((&hot, true)), 35);
        let colored: Vec<_> = spans.iter().map(|s| s.style.fg).collect();
        assert_eq!(colored[0], Some(theme.dim), "the heading stays furniture");
        assert_eq!(colored[1], Some(theme.text), "and so does the label");
        assert_eq!(colored[2], Some(theme.warn), "only the reading warns");
    }

    /// Thirty-five degrees and seventy-four must not draw identically, or nothing says
    /// how much headroom is left until the moment it runs out.
    #[test]
    fn a_cool_sensor_sinks_and_a_warm_one_does_not() {
        let theme = Theme::default();
        let at = |celsius: f32| {
            let mut r = reading("CPU", "Package");
            r.celsius = celsius;
            value_color(&r, &theme)
        };
        assert_eq!(at(35.0), theme.dim, "cool");
        assert_eq!(at(44.0), theme.dim, "still under warn * COOL");
        assert_eq!(at(46.0), theme.text, "warming");
        assert_eq!(at(74.0), theme.text, "warm but under the limit");
        assert_eq!(at(80.0), theme.warn);
        assert_eq!(at(95.0), theme.crit);
    }

    /// Every cell is the same width, or the columns stop lining up.
    #[test]
    fn a_grid_cell_is_always_its_column_width() {
        let theme = Theme::default();
        let r = reading("MB", "Motherboard");
        for width in [20, 24, 35, 60] {
            let drawn: String = cell_spans(&theme, Some((&r, true)), width)
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            assert_eq!(drawn.chars().count(), width, "width {width}");
        }
        // And an empty slot fills its column too.
        let blank: String = cell_spans(&theme, None, 35)
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(blank.chars().count(), 35);
    }

    /// Nothing to draw must still say so rather than leaving the panel blank.
    #[test]
    fn an_empty_panel_says_why() {
        let config = SensorsConfig::default();
        let empty = SensorStore::default();
        assert!(text(&empty, &config, 70)[0].contains("no temperature sensors"));

        let broken = SensorStore {
            error: Some("no hwmon".into()),
            ..SensorStore::default()
        };
        assert!(text(&broken, &config, 70)[0].contains("sensors unavailable"));
    }
}
