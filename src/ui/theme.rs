//! One place for colours and icons.
//!
//! Never hard-code an icon elsewhere. Keeping them here means the ASCII
//! fallback for systems without a Nerd Font is a single switch.

use ratatui::style::Color;

use crate::config::{ColorName, FrameStyle, PartColors, ThemeConfig};
use crate::sources::power::PowerAction;

/// The blocks that can be given colours of their own under `[theme.<block>]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Clock,
    Workspaces,
    Audio,
    Media,
    Sensors,
    Network,
    Power,
}

impl Part {
    pub const COUNT: usize = 7;
}

/// One block's colours as parsed, `None` wherever `[theme]` decides.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Overrides {
    accent: Option<Color>,
    text: Option<Color>,
    dim: Option<Color>,
    warn: Option<Color>,
    crit: Option<Color>,
    sparkline: Option<Color>,
}

impl Overrides {
    fn from_config(config: &PartColors) -> Self {
        let parse = |c: &Option<ColorName>| c.as_ref().and_then(ColorName::parse);
        Self {
            accent: parse(&config.accent),
            text: parse(&config.text),
            dim: parse(&config.dim),
            warn: parse(&config.warn),
            crit: parse(&config.crit),
            sparkline: parse(&config.sparkline),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub accent: Color,
    pub dim: Color,
    pub text: Color,
    pub warn: Color,
    pub crit: Color,
    pub border: Color,
    /// The trend lines.
    pub sparkline: Color,
    /// When false, the glyphs fall back to ASCII.
    pub nerd_font: bool,
    /// How many sides of the outer frame are drawn.
    pub frame: FrameStyle,
    /// `[theme] sparkline` as written. Unset, a block's trend follows that block's
    /// accent rather than the shared one.
    pub sparkline_set: Option<Color>,
    /// Indexed by `Part`.
    pub parts: [Overrides; Part::COUNT],
}

impl Theme {
    /// Build the colours from the config. An unparsable value falls back to the default
    /// rather than refusing to start; in a block's section, to the shared colour.
    pub fn from_config(config: &ThemeConfig) -> Self {
        let d = Theme::default();
        let accent = config.accent.parse().unwrap_or(d.accent);
        let sparkline_set = config.sparkline.as_ref().and_then(ColorName::parse);
        Self {
            accent,
            text: config.text.parse().unwrap_or(d.text),
            dim: config.dim.parse().unwrap_or(d.dim),
            warn: config.warn.parse().unwrap_or(d.warn),
            crit: config.crit.parse().unwrap_or(d.crit),
            border: config.border.parse().unwrap_or(d.border),
            sparkline: sparkline_set.unwrap_or(accent),
            nerd_font: config.nerd_font,
            frame: config.frame,
            sparkline_set,
            parts: [
                &config.clock,
                &config.workspaces,
                &config.audio,
                &config.media,
                &config.sensors,
                &config.network,
                &config.power,
            ]
            .map(Overrides::from_config),
        }
    }

    /// The theme one block draws with: its own colours where it has them, the shared
    /// ones everywhere else.
    ///
    /// A trend line takes, in order, the block's `sparkline`, the shared `sparkline`,
    /// the block's `accent` and the shared `accent` - so giving a block an accent
    /// recolours its trend too, unless a sparkline colour was asked for by name.
    pub fn part(&self, part: Part) -> Theme {
        let o = self.parts[part as usize];
        let accent = o.accent.unwrap_or(self.accent);
        Theme {
            accent,
            text: o.text.unwrap_or(self.text),
            dim: o.dim.unwrap_or(self.dim),
            warn: o.warn.unwrap_or(self.warn),
            crit: o.crit.unwrap_or(self.crit),
            border: self.border,
            sparkline: o.sparkline.or(self.sparkline_set).unwrap_or(accent),
            nerd_font: self.nerd_font,
            frame: self.frame,
            sparkline_set: self.sparkline_set,
            parts: self.parts,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            accent: Color::Cyan,
            dim: Color::DarkGray,
            text: Color::Gray,
            warn: Color::Yellow,
            crit: Color::Red,
            border: Color::DarkGray,
            sparkline: Color::Cyan,
            nerd_font: true,
            frame: FrameStyle::Full,
            sparkline_set: None,
            parts: [Overrides::default(); Part::COUNT],
        }
    }
}

/// One place for the glyphs.
///
/// **Never hard-code an icon elsewhere.** Systems without a Nerd Font, and
/// narrowing down font differences, both stay a single switch here.
impl Theme {
    /// The transport glyphs, all from the Nerd Font's Material Design range.
    ///
    /// Not the Unicode media controls (`⏮` U+23EE and friends): a Nerd Font does
    /// **not** carry most of them, so fontconfig fills them in from an emoji font and
    /// they come out at mismatched sizes. One range means one drawing.
    pub fn icon_media_prev(&self) -> &'static str {
        if self.nerd_font { "󰒮" } else { "<<" }
    }
    pub fn icon_media_next(&self) -> &'static str {
        if self.nerd_font { "󰒭" } else { ">>" }
    }
    pub fn icon_playing(&self) -> &'static str {
        if self.nerd_font { "󰐊" } else { ">" }
    }
    pub fn icon_paused(&self) -> &'static str {
        if self.nerd_font { "󰏤" } else { "||" }
    }
    pub fn icon_note(&self) -> &'static str {
        if self.nerd_font { "󰎇" } else { "*" }
    }

    /// The marks that introduce the mpris metadata fields, or `None` where there is no
    /// font for them - then the fields are joined by ` / ` instead.
    pub fn icon_track(&self) -> Option<&'static str> {
        self.nerd_font.then_some("󰎇")
    }
    pub fn icon_artist(&self) -> Option<&'static str> {
        self.nerd_font.then_some("󰀄")
    }
    pub fn icon_album(&self) -> Option<&'static str> {
        self.nerd_font.then_some("󰀥")
    }
    /// The audio row's own glyphs. Mute is said by **swapping the icon**, not by a
    /// badge beside it: a badge would need click columns of its own, and the icon is
    /// already the thing you click.
    pub fn icon_volume(&self) -> &'static str {
        if self.nerd_font { "󰕾" } else { "Out:" }
    }
    pub fn icon_volume_muted(&self) -> &'static str {
        if self.nerd_font { "󰝟" } else { "Out!" }
    }
    pub fn icon_mic(&self) -> &'static str {
        if self.nerd_font { "󰍬" } else { "In:" }
    }
    pub fn icon_mic_muted(&self) -> &'static str {
        if self.nerd_font { "󰍭" } else { "In!" }
    }
    /// The interface-kind marks. `sources::network::Kind` is the one classifier behind
    /// both these and the sort order, so an icon can never disagree with the ordering.
    pub fn icon_net(&self, kind: crate::sources::network::Kind) -> &'static str {
        use crate::sources::network::Kind;
        match (kind, self.nerd_font) {
            (Kind::Wireless, true) => "󰖩",
            (Kind::Wireless, false) => "w",
            (Kind::Wired, true) => "󰈀",
            (Kind::Wired, false) => "e",
            (Kind::Virtual, true) => "󰘘",
            (Kind::Virtual, false) => "v",
        }
    }

    /// The mark on the interface selector that introduces the count of what did not fit.
    pub fn icon_more(&self) -> &'static str {
        if self.nerd_font { "󰅂" } else { ">" }
    }

    /// The mark the bar ends on, standing in for the whole power row.
    pub fn icon_power(&self) -> &'static str {
        if self.nerd_font { "⏻" } else { "PWR" }
    }

    /// The power row's buttons, one glyph each, from the same Material Design range
    /// as the transport row.
    ///
    /// Spelled out as `[ Screen Off ]   [ Suspend ]   [ Power Off ]` they take 44 columns
    /// of the bottom row for three things that are already distinct shapes. Without a
    /// Nerd Font there is no shape to fall back on, so the
    /// words come back - the row has the width for them either way.
    pub fn icon_power_action(&self, action: PowerAction) -> &'static str {
        match (action, self.nerd_font) {
            (PowerAction::ScreenOff, true) => "\u{f0379}",
            (PowerAction::ScreenOff, false) => "[ Screen Off ]",
            (PowerAction::Suspend, true) => "\u{f04b2}",
            (PowerAction::Suspend, false) => "[ Suspend ]",
            (PowerAction::PowerOff, true) => "\u{f0425}",
            (PowerAction::PowerOff, false) => "[ Power Off ]",
        }
    }

    pub fn icon_down(&self) -> &'static str {
        if self.nerd_font { "↓" } else { "v" }
    }
    pub fn icon_up(&self) -> &'static str {
        if self.nerd_font { "↑" } else { "^" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ColorName;

    #[test]
    fn parses_named_colors() {
        assert_eq!(ColorName("cyan".into()).parse(), Some(Color::Cyan));
        assert_eq!(ColorName("DarkGray".into()).parse(), Some(Color::DarkGray));
        assert_eq!(ColorName("grey".into()).parse(), Some(Color::Gray));
    }

    #[test]
    fn parses_hex_and_indexed() {
        assert_eq!(
            ColorName("#ff8800".into()).parse(),
            Some(Color::Rgb(0xff, 0x88, 0x00))
        );
        assert_eq!(ColorName("42".into()).parse(), Some(Color::Indexed(42)));
    }

    /// Every glyph must have an ASCII fallback.
    #[test]
    fn ascii_fallbacks_exist_for_every_icon() {
        let ascii = Theme {
            nerd_font: false,
            ..Theme::default()
        };
        let nerd = Theme::default();
        let pairs: Vec<(&str, &str)> = vec![
            (nerd.icon_media_prev(), ascii.icon_media_prev()),
            (nerd.icon_media_next(), ascii.icon_media_next()),
            (nerd.icon_playing(), ascii.icon_playing()),
            (nerd.icon_paused(), ascii.icon_paused()),
            (nerd.icon_note(), ascii.icon_note()),
            (nerd.icon_volume(), ascii.icon_volume()),
            (nerd.icon_volume_muted(), ascii.icon_volume_muted()),
            (nerd.icon_mic(), ascii.icon_mic()),
            (nerd.icon_mic_muted(), ascii.icon_mic_muted()),
            (nerd.icon_down(), ascii.icon_down()),
            (nerd.icon_up(), ascii.icon_up()),
            (nerd.icon_more(), ascii.icon_more()),
            (
                nerd.icon_power_action(PowerAction::ScreenOff),
                ascii.icon_power_action(PowerAction::ScreenOff),
            ),
            (
                nerd.icon_power_action(PowerAction::Suspend),
                ascii.icon_power_action(PowerAction::Suspend),
            ),
            (
                nerd.icon_power_action(PowerAction::PowerOff),
                ascii.icon_power_action(PowerAction::PowerOff),
            ),
        ];
        for (n, a) in pairs {
            assert_ne!(n, a, "fallback must differ from the nerd font glyph");
            assert!(a.is_ascii(), "fallback {a:?} must be ASCII");
            assert!(!a.is_empty());
        }
    }

    /// The mpris field marks have no ASCII spelling: without a Nerd Font the fields go
    /// back to being separated by slashes, and `None` is what says so.
    #[test]
    fn the_field_marks_fall_back_to_nothing() {
        let ascii = Theme {
            nerd_font: false,
            ..Theme::default()
        };
        let nerd = Theme::default();
        for (n, a) in [
            (nerd.icon_track(), ascii.icon_track()),
            (nerd.icon_artist(), ascii.icon_artist()),
            (nerd.icon_album(), ascii.icon_album()),
        ] {
            assert!(n.is_some());
            assert_eq!(a, None);
        }
        // Three fields, three different marks - or they say nothing.
        let marks = [nerd.icon_track(), nerd.icon_artist(), nerd.icon_album()];
        for (i, one) in marks.iter().enumerate() {
            for other in &marks[i + 1..] {
                assert_ne!(one, other);
            }
        }
    }

    /// The transport glyphs have to be distinguishable from each other; `icon_playing`
    /// and `icon_paused` sit in the same cell at different times.
    #[test]
    fn the_transport_glyphs_are_all_different() {
        for theme in [
            Theme::default(),
            Theme {
                nerd_font: false,
                ..Theme::default()
            },
        ] {
            let all = [
                theme.icon_media_prev(),
                theme.icon_media_next(),
                theme.icon_playing(),
                theme.icon_paused(),
            ];
            for (i, one) in all.iter().enumerate() {
                for other in &all[i + 1..] {
                    assert_ne!(one, other, "nerd_font={}", theme.nerd_font);
                }
            }
        }
    }

    /// The three power glyphs sit side by side, so they have to be distinguishable -
    /// and one of them powers the machine off.
    #[test]
    fn the_power_glyphs_are_all_different() {
        for theme in [
            Theme::default(),
            Theme {
                nerd_font: false,
                ..Theme::default()
            },
        ] {
            let all = [
                theme.icon_power_action(PowerAction::ScreenOff),
                theme.icon_power_action(PowerAction::Suspend),
                theme.icon_power_action(PowerAction::PowerOff),
            ];
            for (i, one) in all.iter().enumerate() {
                for other in &all[i + 1..] {
                    assert_ne!(one, other, "nerd_font={}", theme.nerd_font);
                }
            }
        }
    }

    /// A broken value must fall back to the default instead of refusing to start.
    #[test]
    fn unknown_color_falls_back_to_the_default() {
        assert_eq!(ColorName("chartreuse".into()).parse(), None);
        assert_eq!(ColorName("#xyz".into()).parse(), None);

        let cfg = ThemeConfig {
            accent: ColorName("chartreuse".into()),
            ..ThemeConfig::default()
        };
        assert_eq!(Theme::from_config(&cfg).accent, Theme::default().accent);
    }

    fn colour(name: &str) -> Option<ColorName> {
        Some(ColorName(name.into()))
    }

    /// A block takes the colours it names and the shared ones for the rest, and no
    /// other block sees its colours.
    #[test]
    fn a_block_overrides_only_what_it_names() {
        let cfg = ThemeConfig {
            clock: PartColors {
                accent: colour("magenta"),
                ..PartColors::default()
            },
            ..ThemeConfig::default()
        };
        let theme = Theme::from_config(&cfg);
        let clock = theme.part(Part::Clock);
        assert_eq!(clock.accent, Color::Magenta);
        assert_eq!(clock.text, theme.text);
        assert_eq!(clock.border, theme.border);
        assert_eq!(theme.part(Part::Audio).accent, Color::Cyan);
        assert_eq!(theme.accent, Color::Cyan, "the shared colour is untouched");
    }

    /// The trend follows the block's accent until a sparkline colour is named, and a
    /// block's own sparkline beats the shared one.
    #[test]
    fn the_sparkline_falls_back_through_the_accents() {
        let mut cfg = ThemeConfig {
            sensors: PartColors {
                accent: colour("green"),
                ..PartColors::default()
            },
            ..ThemeConfig::default()
        };
        let theme = Theme::from_config(&cfg);
        assert_eq!(theme.sparkline, Color::Cyan, "unset follows the accent");
        assert_eq!(theme.part(Part::Sensors).sparkline, Color::Green);

        cfg.sparkline = colour("blue");
        let theme = Theme::from_config(&cfg);
        assert_eq!(theme.part(Part::Sensors).sparkline, Color::Blue);
        assert_eq!(theme.part(Part::Network).sparkline, Color::Blue);

        cfg.network.sparkline = colour("red");
        let theme = Theme::from_config(&cfg);
        assert_eq!(theme.part(Part::Network).sparkline, Color::Red);
        assert_eq!(theme.part(Part::Sensors).sparkline, Color::Blue);
    }

    /// A broken colour in a block's section falls back to the shared one, not to the
    /// built-in default.
    #[test]
    fn a_broken_block_colour_falls_back_to_the_shared_one() {
        let cfg = ThemeConfig {
            accent: colour("yellow").unwrap(),
            media: PartColors {
                accent: colour("chartreuse"),
                ..PartColors::default()
            },
            ..ThemeConfig::default()
        };
        assert_eq!(
            Theme::from_config(&cfg).part(Part::Media).accent,
            Color::Yellow
        );
    }
}
