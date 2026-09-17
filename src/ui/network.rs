//! The network rows.
//!
//! **One interface at a time, chosen on the selector row.** The pane was never wide
//! enough for two - a wireless block alone took three of its four rows - so what used
//! to happen was that the list scrolled and a row at the bottom named what had fallen
//! off. Showing one on purpose costs nothing that was ever visible and buys the room
//! to stack rx over tx, which is what lets a sparkline be wide enough to hold more
//! than the last forty seconds.
//!
//! The five rows are fixed whatever is selected: identity, link, rx, tx, selector. A
//! wired interface has nothing of the link row's shape to say and leaves it blank,
//! because a pane that changes height as you click through it is worse than one with
//! a gap in it.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::sources::network::{Interface, Security};
use crate::store::NetworkStore;
use crate::ui::hit::Action;
use crate::ui::slot::{DrawCtx, Measure, Rendered, Slot, priority};
use crate::ui::theme::{Part, Theme};
use crate::util::sparkline::{Ring, Scale};
use crate::util::text::fit;

/// The pane's left margin, shared with everything else on screen.
const INDENT: usize = 2;
/// Columns the interface name gets. `veth1a2b3c4` is eleven.
const NAME_WIDTH: usize = 11;
/// Columns the operstate gets, on the rows that show one.
const STATE_WIDTH: usize = 10;
/// The link row sits under the **name**, not under the icon: the indent is what says
/// it describes the interface above it.
const LINK_INDENT: usize = INDENT + 2;
/// Room kept back on the selector row for the `>N` count.
const MORE_WIDTH: usize = 4;

/// Traffic is read as a magnitude and spans orders of magnitude, so the scale is
/// logarithmic and anchored rather than fitted to the window - see `Scale::Log`.
///
/// The floor is 1 KiB/s: below that a link is doing nothing worth a bar. Two decades
/// keeps a quiet window from being stretched over the full height, so the difference
/// between an idle link and a busy one stays visible in the height itself.
const TRAFFIC: Scale = Scale::Log {
    floor: 1024.0,
    min_decades: 2.0,
};

/// Signal strength bar. Buckets RSSI (dBm) into four levels.
///
/// Rule of thumb: >= -50 excellent, -60 good, -70 fair, below that poor.
pub fn signal_bars(rssi: i32) -> &'static str {
    match rssi {
        r if r >= -50 => "▂▄▆█",
        r if r >= -60 => "▂▄▆ ",
        r if r >= -70 => "▂▄  ",
        _ => "▂   ",
    }
}

/// Format bytes per second for humans.
pub fn human_rate(bytes_per_sec: f64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes_per_sec.max(0.0);
    if b >= GB {
        format!("{:.1} GB/s", b / GB)
    } else if b >= MB {
        format!("{:.1} MB/s", b / MB)
    } else if b >= KB {
        format!("{:.0} kB/s", b / KB)
    } else {
        format!("{b:.0} B/s")
    }
}

/// Colour for operstate. Anything but `up` is not usable yet, so it warns.
fn state_color(operstate: &str, theme: &Theme) -> ratatui::style::Color {
    match operstate {
        "up" => theme.accent,
        "dormant" | "testing" => theme.warn,
        _ => theme.dim,
    }
}

/// Which of the four strength steps an RSSI falls in, 1..=4.
///
/// Rule of thumb: >= -50 excellent, -60 good, -70 fair, below that poor.
pub fn signal_level(rssi: i32) -> u8 {
    match rssi {
        r if r >= -50 => 4,
        r if r >= -60 => 3,
        r if r >= -70 => 2,
        _ => 1,
    }
}

/// Split a formatted rate into its number and its unit, so the two can be coloured
/// apart: the number is the reading, the unit is furniture.
fn split_unit(rate: &str) -> (&str, &str) {
    match rate.rsplit_once(' ') {
        Some((value, unit)) => (value, unit),
        None => (rate, ""),
    }
}

/// The band a frequency belongs to, or `None` for one that cannot be placed.
///
/// Deliberately three bands and no arithmetic beyond them: a channel outside the
/// ranges we know is shown as its raw megahertz rather than filed under the nearest
/// label, which is the same rule the sensor names follow.
pub fn band(freq_mhz: u32) -> Option<&'static str> {
    match freq_mhz {
        2400..=2500 => Some("2.4G"),
        4900..=5900 => Some("5G"),
        5925..=7125 => Some("6G"),
        _ => None,
    }
}

/// The character a redacted reading is drawn with.
const MASK: char = '•';

/// Cover a value up, keeping the columns it took.
///
/// Same width, so pressing the key does not move anything on screen - the picture you
/// take is the layout you were looking at.
fn mask(text: &str) -> String {
    MASK.to_string().repeat(crate::util::text::width(text))
}

/// Cover an address up, but keep what is not worth hiding.
///
/// The dots and the prefix length stay: `/24` is the most ordinary thing about a home
/// network and says nothing about whose it is, while leaving the shape recognisable as
/// an address rather than as a rendering fault.
fn mask_address(ip: &str) -> String {
    let (addr, prefix) = match ip.split_once('/') {
        Some((addr, prefix)) => (addr, Some(prefix)),
        None => (ip, None),
    };
    let masked: Vec<String> = addr.split('.').map(mask).collect();
    match prefix {
        Some(prefix) => format!("{}/{prefix}", masked.join(".")),
        None => masked.join("."),
    }
}

/// The identity row: what this interface is, and whether it can be reached.
fn identity_row<'a>(theme: &Theme, iface: &Interface, width: usize, redact: bool) -> Line<'a> {
    let mut spans = vec![
        Span::raw(" ".repeat(INDENT)),
        Span::styled(
            format!("{} ", theme.icon_net(iface.kind)),
            Style::default().fg(state_color(&iface.operstate, theme)),
        ),
        Span::styled(
            fit(&iface.name, NAME_WIDTH),
            Style::default().fg(theme.text),
        ),
        Span::raw("  "),
    ];
    // `up` is the boring case and the icon's colour already says it. Anything else is
    // worth a word: `DORMANT` is wpa authenticating just after boot, and a reader who
    // sees it knows to wait rather than to investigate.
    if iface.operstate != "up" {
        spans.push(Span::styled(
            fit(&iface.operstate.to_uppercase(), STATE_WIDTH),
            Style::default()
                .fg(state_color(&iface.operstate, theme))
                .add_modifier(Modifier::BOLD),
        ));
    }
    let used: usize = spans
        .iter()
        .map(|s| crate::util::text::width(&s.content))
        .sum();
    match &iface.ipv4 {
        Some(ip) => spans.push(Span::styled(
            fit(
                &if redact { mask_address(ip) } else { ip.clone() },
                width.saturating_sub(used),
            ),
            Style::default().fg(theme.text),
        )),
        None => spans.push(Span::styled(
            fit("(no address yet)", width.saturating_sub(used)),
            Style::default().fg(theme.dim),
        )),
    }
    Line::from(spans)
}

/// The link row: what this interface is attached to, and how well.
///
/// Indented under the name rather than under the icon, which is what says "this
/// describes the thing above" without spending a column on a mark.
///
/// Wireless only. A wired or virtual interface has nothing of this shape to say, so it
/// gets a blank row: the block stays the same height whatever is selected, and a pane
/// that changes height as you click through it is worse than one with a gap in it.
fn link_row<'a>(theme: &Theme, iface: &Interface, width: usize, redact: bool) -> Line<'a> {
    let Some(w) = &iface.wireless else {
        return Line::raw("");
    };
    if w.state.as_deref() != Some("COMPLETED") {
        return Line::styled(
            format!(
                "{}{}",
                " ".repeat(LINK_INDENT),
                w.state.as_deref().unwrap_or("not connected")
            ),
            Style::default().fg(theme.warn),
        );
    }

    // Everything but the SSID is fixed width, so the SSID gets what is left. That is
    // the whole point of dropping the frequency, the suite and the dBm reading.
    let mut tail: Vec<Span> = Vec::new();
    if let Some(f) = w.freq_mhz {
        tail.push(Span::styled(
            match band(f) {
                Some(name) => format!("  {name:<4}"),
                None => format!("  {f:<4}"),
            },
            Style::default().fg(theme.dim),
        ));
    }
    // The suite is configuration, not state, and does not change while you are on the
    // network - so it is only worth a column when it is worth worrying about.
    if let Some(security) = w.security() {
        let warning = match security {
            Security::Current => None,
            Security::Open => Some("OPEN".to_string()),
            Security::Wep => Some("WEP".to_string()),
            Security::Unknown(raw) => Some(raw),
        };
        if let Some(text) = warning {
            tail.push(Span::styled(
                format!("  {text}"),
                Style::default().fg(theme.warn).add_modifier(Modifier::BOLD),
            ));
        }
    }
    if let Some(rssi) = w.rssi {
        // Blocks, not an icon. The marks in this pane say *what* a thing is; the
        // blocks say *how much* of it there is, which is what the traffic rows below
        // are drawn with. Signal strength is a measurement, so it belongs to that
        // vocabulary - and a Material Design glyph is drawn at the height of the text
        // around it, which is far too small a thing to read at a glance.
        let level = signal_level(rssi);
        tail.push(Span::styled(
            format!("  {}", signal_bars(rssi)),
            Style::default().fg(if level >= 2 { theme.accent } else { theme.warn }),
        ));
    }
    if let Some(mbps) = w.link_mbps {
        tail.push(Span::styled(
            format!("  {mbps:>4}"),
            Style::default().fg(theme.text),
        ));
        tail.push(Span::styled(" Mb/s", Style::default().fg(theme.dim)));
    }

    let tail_width: usize = tail
        .iter()
        .map(|s| crate::util::text::width(&s.content))
        .sum();
    let ssid_width = width.saturating_sub(LINK_INDENT + tail_width);
    let ssid = w.ssid.as_deref().unwrap_or("(unknown ssid)");
    let mut spans = vec![
        Span::raw(" ".repeat(LINK_INDENT)),
        Span::styled(
            fit(
                &if redact { mask(ssid) } else { ssid.to_string() },
                ssid_width,
            ),
            Style::default().fg(theme.text),
        ),
    ];
    spans.extend(tail);
    Line::from(spans)
}

/// One traffic row. Rx and tx get one each, stacked, so both can be as wide as the
/// pane instead of splitting it between them.
fn traffic_row<'a>(
    theme: &Theme,
    mark: &str,
    rate: f64,
    history: Option<&Ring>,
    spark_width: usize,
) -> Line<'a> {
    let formatted = human_rate(rate);
    let (value, unit) = split_unit(&formatted);
    let drawn = history
        .map(|r| r.render(spark_width, TRAFFIC))
        .unwrap_or_default();
    Line::from(vec![
        Span::styled(
            format!("{}{mark} ", " ".repeat(INDENT)),
            Style::default().fg(theme.dim),
        ),
        Span::styled(format!("{value:>7}"), Style::default().fg(theme.text)),
        Span::styled(format!(" {unit:<5}"), Style::default().fg(theme.dim)),
        // Padding the short series out to the field is the caller's job - `render`
        // returns one character per sample, so without this the columns would shift
        // while the ring fills up.
        Span::styled(
            format!("{drawn:<spark_width$}"),
            Style::default().fg(theme.sparkline),
        ),
    ])
}

/// The selector: every interface, the current one marked, and a count of the rest.
///
/// It is a control and a readout at once. With `input.mouse = false` nothing here can
/// be clicked, and then this row is the only thing that still says what else the
/// machine has - so what does not fit is counted rather than simply dropped.
fn selector_row<'a>(
    theme: &Theme,
    interfaces: &[Interface],
    selected: &str,
    area: Rect,
    row: u16,
    hits: &mut Vec<(Rect, Action)>,
) -> Line<'a> {
    // The selected one is never the one that gets dropped: a selector that hides what
    // you are looking at has stopped being a selector.
    let mut order: Vec<usize> = (0..interfaces.len()).collect();
    order.sort_by_key(|i| (interfaces[*i].name != selected, *i));

    let budget = (area.width as usize).saturating_sub(INDENT + MORE_WIDTH);
    let mut shown: Vec<usize> = Vec::new();
    let mut used = 0;
    for i in order {
        let cost = INDENT + 2 + crate::util::text::width(&interfaces[i].name);
        if used + cost > budget && !shown.is_empty() {
            continue;
        }
        used += cost;
        shown.push(i);
    }
    // Back into display order, so the row does not reshuffle as the selection moves.
    shown.sort_unstable();

    let mut spans = vec![Span::raw(" ".repeat(INDENT))];
    let mut x = area.x + INDENT as u16;
    for i in &shown {
        let iface = &interfaces[*i];
        let current = iface.name == selected;
        let text = format!("{} {}", theme.icon_net(iface.kind), iface.name);
        let style = if current {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.dim)
        };
        let cell = format!("{text}  ");
        let cell_width = crate::util::text::width(&cell) as u16;
        // The index is into the interface list itself, not into the row as drawn: the
        // reducer sees that same list, and it would have to reproduce this row's
        // width arithmetic to make sense of a position on screen.
        hits.push((
            Rect::new(x, row, cell_width, 1),
            Action::NetworkSelect { index: *i as u8 },
        ));
        x += cell_width;
        spans.push(Span::styled(cell, style));
    }
    let hidden = interfaces.len() - shown.len();
    if hidden > 0 {
        spans.push(Span::styled(
            format!("{}{hidden}", theme.icon_more()),
            Style::default().fg(theme.dim),
        ));
    }
    Line::from(spans)
}

/// How wide the sparklines can be, given the pane.
///
/// Derived rather than fixed: a narrower terminal has to shrink, and there is no point
/// drawing more columns than the ring can ever hold - at the default 60 samples and a
/// five second tick that is five minutes of history, and the field tops out there.
fn spark_width(area: Rect, history_len: usize) -> usize {
    let fixed = INDENT + 2 + 7 + 6 + INDENT;
    (area.width as usize).saturating_sub(fixed).min(history_len)
}

/// Which interface the pane is showing.
///
/// The selection is held by name, not by position: `order` sorts docker's interfaces
/// in among the real ones, so a container starting would otherwise slide the selection
/// onto a different interface without anyone touching anything. A name that is no
/// longer there falls back to the first in the current order, which `rank` has already
/// made the most useful one.
/// What the network pane reads.
pub struct NetworkSlot<'a> {
    pub network: &'a NetworkStore,
    pub history_len: usize,
}

impl NetworkSlot<'_> {
    fn selected<'a>(&self, interfaces: &'a [Interface]) -> Option<&'a Interface> {
        self.network
            .selected
            .as_deref()
            .and_then(|name| interfaces.iter().find(|i| i.name == name))
            .or_else(|| interfaces.first())
    }
}

impl Slot for NetworkSlot<'_> {
    fn part(&self) -> Part {
        Part::Network
    }

    /// One interface takes five rows - identity, link, rx, tx, and the selector.
    fn measure(&self, _width: u16) -> Measure {
        let interfaces = &self.network.latest.interfaces;
        if interfaces.is_empty() {
            return Measure::fixed(1, priority::NETWORK);
        }
        Measure {
            // Below the identity row there is nothing worth keeping.
            min: 1,
            preferred: if interfaces.len() > 1 { 5 } else { 4 },
            priority: priority::NETWORK,
        }
    }

    /// `area.height` is the budget, and the selector registers its hits against `area`.
    fn render(&self, ctx: &DrawCtx, area: Rect) -> Rendered<'static> {
        let theme = ctx.theme;
        let mut hits: Vec<(Rect, Action)> = Vec::new();
        if let Some(err) = &self.network.error
            && self.network.latest.interfaces.is_empty()
        {
            return Rendered::new(vec![Line::styled(
                format!("  network unavailable: {err}"),
                Style::default().fg(theme.dim),
            )]);
        }
        let interfaces = &self.network.latest.interfaces;
        let Some(iface) = self.selected(interfaces) else {
            return Rendered::new(vec![Line::styled(
                "  no active interfaces",
                Style::default().fg(theme.dim),
            )]);
        };

        let width = (area.width as usize).saturating_sub(INDENT);
        let spark = spark_width(area, self.history_len);
        let (rx, tx) = self
            .network
            .rates
            .get(&iface.name)
            .copied()
            .unwrap_or((0.0, 0.0));

        let mut rows = vec![
            identity_row(theme, iface, width, self.network.redact),
            link_row(theme, iface, width, self.network.redact),
            traffic_row(
                theme,
                theme.icon_down(),
                rx,
                self.network.rx_history.get(&iface.name),
                spark,
            ),
            traffic_row(
                theme,
                theme.icon_up(),
                tx,
                self.network.tx_history.get(&iface.name),
                spark,
            ),
        ];

        // With a single row to give, it goes to the identity: which interface, and its
        // address. A selector on its own says what could be shown and nothing about
        // what is, which is the wrong half to keep.
        let budget = area.height as usize;
        if budget == 1 {
            rows.truncate(1);
            return Rendered { lines: rows, hits };
        }

        // The selector keeps the last row for itself, so a short pane loses detail
        // rather than losing the only way to reach the other interfaces.
        if interfaces.len() > 1 && budget > 0 {
            rows.truncate(budget - 1);
            let row = area.y + rows.len() as u16;
            rows.push(selector_row(
                theme,
                interfaces,
                &iface.name,
                area,
                row,
                &mut hits,
            ));
        } else {
            rows.truncate(budget);
        }
        Rendered { lines: rows, hits }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_bars_are_always_four_columns() {
        for rssi in [-20, -50, -51, -60, -61, -70, -71, -95] {
            assert_eq!(signal_bars(rssi).chars().count(), 4, "rssi {rssi}");
        }
    }

    #[test]
    fn stronger_signal_shows_more_bars() {
        let filled = |s: &str| s.chars().filter(|c| *c != ' ').count();
        assert!(filled(signal_bars(-45)) > filled(signal_bars(-55)));
        assert!(filled(signal_bars(-55)) > filled(signal_bars(-65)));
        assert!(filled(signal_bars(-65)) > filled(signal_bars(-85)));
    }

    /// The band is the part worth a column; the exact channel is diagnostic detail.
    #[test]
    fn frequencies_become_bands() {
        assert_eq!(band(2412), Some("2.4G"));
        assert_eq!(band(2484), Some("2.4G"));
        assert_eq!(band(5180), Some("5G"));
        assert_eq!(band(5825), Some("5G"));
        assert_eq!(band(6115), Some("6G"));
    }

    /// A channel outside the bands we know is shown as it came rather than filed under
    /// the nearest label - the same rule the sensor names follow.
    #[test]
    fn an_unplaceable_frequency_is_not_guessed_at() {
        assert_eq!(band(900), None);
        assert_eq!(band(60_000), None);
        assert_eq!(band(0), None);
    }

    /// The marks in this pane say what a thing is; the blocks say how much of it there
    /// is. Signal strength is a measurement, so it is drawn the way the traffic is.
    #[test]
    fn the_signal_is_drawn_in_blocks_not_in_an_icon() {
        let theme = Theme::default();
        let mut iface = wireless_iface();
        iface.wireless.as_mut().unwrap().rssi = Some(-45);
        let drawn: String = link_row(&theme, &iface, 60, false)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(drawn.contains(signal_bars(-45)), "{drawn:?}");
        // Four columns of it, whatever the font: there is no Nerd Font variant.
        let ascii = Theme {
            nerd_font: false,
            ..Theme::default()
        };
        let plain: String = link_row(&ascii, &iface, 60, false)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(plain.contains(signal_bars(-45)), "{plain:?}");
    }

    /// A network worth worrying about says so; a current one says nothing at all.
    #[test]
    fn only_a_risky_suite_takes_a_column() {
        let theme = Theme::default();
        let text = |key: &str, cipher: &str| {
            let mut iface = wireless_iface();
            let w = iface.wireless.as_mut().unwrap();
            w.key_mgmt = Some(key.into());
            w.cipher = Some(cipher.into());
            link_row(&theme, &iface, 60, false)
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        assert!(
            !text("WPA2-PSK", "CCMP").contains("WPA2"),
            "the current case is silent"
        );
        assert!(text("NONE", "CCMP").contains("OPEN"));
        assert!(text("NONE", "WEP-104").contains("WEP"));
        assert!(
            text("WPA-PSK", "TKIP").contains("WPA-PSK"),
            "an unknown suite is shown raw"
        );
    }

    fn wireless_iface() -> Interface {
        use crate::sources::network::{Kind, Wireless};
        Interface {
            kind: Kind::Wireless,
            name: "wlp11s0".into(),
            operstate: "up".into(),
            ipv4: Some("192.168.1.23/24".into()),
            wireless: Some(Wireless {
                ssid: Some("MyNetwork-5G".into()),
                freq_mhz: Some(5180),
                key_mgmt: Some("WPA2-PSK".into()),
                cipher: Some("CCMP".into()),
                rssi: Some(-51),
                link_mbps: Some(173),
                state: Some("COMPLETED".into()),
            }),
            rx_bytes: 0,
            tx_bytes: 0,
        }
    }

    #[test]
    fn signal_levels_climb_with_the_signal() {
        assert_eq!(signal_level(-45), 4);
        assert_eq!(signal_level(-55), 3);
        assert_eq!(signal_level(-65), 2);
        assert_eq!(signal_level(-85), 1);
    }

    /// The number is the reading and the unit is furniture, so they are coloured apart.
    #[test]
    fn a_rate_splits_into_its_number_and_its_unit() {
        assert_eq!(split_unit("12 kB/s"), ("12", "kB/s"));
        assert_eq!(split_unit("1.0 MB/s"), ("1.0", "MB/s"));
        assert_eq!(split_unit("nounit"), ("nounit", ""));
    }

    /// Wide enough to be worth stacking rx over tx, and never wider than the ring can
    /// fill - at 60 samples and a five second tick, five minutes is the ceiling.
    #[test]
    fn the_sparkline_takes_the_width_that_is_left() {
        let pane = Rect::new(0, 0, 70, 5);
        assert_eq!(spark_width(pane, 60), 51);
        // The ring is the other limit.
        assert_eq!(spark_width(pane, 20), 20);
        // A narrow pane shrinks instead of overflowing.
        assert_eq!(spark_width(Rect::new(0, 0, 24, 5), 60), 5);
        assert_eq!(spark_width(Rect::new(0, 0, 8, 5), 60), 0);
    }

    /// The key exists for one moment: you are about to take a picture. It must not
    /// move anything, or the picture is not of what you were looking at.
    #[test]
    fn hiding_a_reading_keeps_its_columns() {
        for text in ["MyNetwork-5G", "HomeNetwork-2G-Guest", "日本語のSSID", ""] {
            assert_eq!(
                crate::util::text::width(&mask(text)),
                crate::util::text::width(text),
                "{text:?}"
            );
        }
        assert_eq!(
            crate::util::text::width(&mask_address("192.168.1.23/24")),
            crate::util::text::width("192.168.1.23/24")
        );
    }

    /// The dots and the prefix stay: `/24` says nothing about whose network it is, and
    /// keeping the shape means the row still reads as an address.
    #[test]
    fn a_hidden_address_still_looks_like_one() {
        assert_eq!(mask_address("192.168.1.23/24"), "•••.•••.•.••/24");
        assert_eq!(mask_address("10.0.0.1"), "••.•.•.•");
        assert_eq!(mask_address("(no address yet)"), "••••••••••••••••");
    }

    /// And nothing of the original survives on screen.
    #[test]
    fn the_ssid_and_the_address_go_together() {
        let theme = Theme::default();
        let iface = wireless_iface();
        let row = |f: fn(&Theme, &Interface, usize, bool) -> Line<'static>, redact| {
            f(&theme, &iface, 60, redact)
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        assert!(row(identity_row, false).contains("192.168.1.23"));
        assert!(!row(identity_row, true).contains("192.168.1.23"));
        assert!(row(identity_row, true).contains(MASK));

        assert!(row(link_row, false).contains("MyNetwork-5G"));
        assert!(!row(link_row, true).contains("MyNetwork-5G"));
        assert!(row(link_row, true).contains(MASK));
        // What is not identifying stays: the band, the signal, the link rate.
        assert!(
            row(link_row, true).contains("5G"),
            "{}",
            row(link_row, true)
        );
        assert!(
            row(link_row, true).contains("173"),
            "{}",
            row(link_row, true)
        );
    }

    #[test]
    fn state_colors_distinguish_up_from_pending() {
        let t = Theme::default();
        assert_eq!(state_color("up", &t), t.accent);
        assert_eq!(state_color("dormant", &t), t.warn);
        assert_ne!(state_color("unknown", &t), t.accent);
    }

    #[test]
    fn human_rate_picks_sensible_units() {
        assert_eq!(human_rate(0.0), "0 B/s");
        assert_eq!(human_rate(512.0), "512 B/s");
        assert_eq!(human_rate(1024.0), "1 kB/s");
        assert_eq!(human_rate(1536.0), "2 kB/s");
        assert_eq!(human_rate(1024.0 * 1024.0), "1.0 MB/s");
        assert_eq!(human_rate(1024.0 * 1024.0 * 1024.0), "1.0 GB/s");
    }

    /// A counter that wrapped must not produce a negative rate.
    #[test]
    fn negative_rate_is_clamped() {
        assert_eq!(human_rate(-5.0), "0 B/s");
    }
}
