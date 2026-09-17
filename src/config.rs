//! The config file. Without one, every setting falls back to its default.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub general: General,
    pub clock: Clock,
    pub workspaces: WorkspacesConfig,
    pub sensors: SensorsConfig,
    pub audio: AudioConfig,
    pub signals: SignalsConfig,
    pub power: PowerConfig,
    pub mpris: MprisConfig,
    pub art: ArtConfig,
    pub network: NetworkConfig,
    pub commands: CommandsConfig,
    pub input: InputConfig,
    pub theme: ThemeConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    /// How often the periodic updates (the clock and friends) run.
    pub tick_ms: u64,
    /// How often temperatures are read; slower than the clock is fine.
    pub sensor_tick_ms: u64,
    /// How often the network is read. It spawns `ip` and `wpa_cli`, so it stays below the temperature rate.
    pub network_tick_ms: u64,
    /// How many samples the sparklines keep.
    pub history_len: usize,
    /// The display mode. `auto` decides from the terminal size.
    pub layout: LayoutSetting,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Clock {
    pub format: String,
    /// The date without the weekday. Kept apart from `weekday_format` so the two can
    /// be drawn in different styles: the date is the reading, the weekday restates it.
    /// One `strftime` string could not be split, because nothing here knows which of
    /// its characters came from `%a`.
    pub date_format: String,
    /// The weekday, drawn as furniture beside `date_format`. Empty leaves it out.
    pub weekday_format: String,
    /// Either "block" (three-line block glyphs) or "plain" (a single line).
    pub style: ClockStyle,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspacesConfig {
    /// Workspaces `1..=count` are always drawn, even before Hyprland has created them
    /// (it only reports the ones that exist). Anything beyond that is still shown
    /// while it exists, so a pinned 10 never disappears.
    pub count: i64,
    /// What is sent to Hyprland's `.socket.sock` after `dispatch ` when a cell is
    /// clicked; `{id}` is replaced with the workspace id. The default is upstream
    /// Hyprland's own syntax; a dispatcher that reads another syntax needs this
    /// overridden.
    pub switch: String,
}

/// `[general] layout`. Anything but `auto` pins the mode regardless of terminal size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutSetting {
    Auto,
    #[serde(rename = "2-pane")]
    TwoPane,
    #[serde(rename = "2-pane-narrow")]
    TwoPaneNarrow,
    #[serde(rename = "1-pane")]
    OnePane,
    Bar,
}

/// `[theme] frame`. How many sides of the outer frame are drawn.
///
/// The frame is chrome a tiling compositor already provides: Hyprland draws its own
/// border and gap around the window, so the full box is the second one. Dropping the
/// sides and the bottom gives the panes a row and two columns back, and the top rule
/// stays because the date is written on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameStyle {
    /// All four sides.
    Full,
    /// The top rule only.
    Top,
}

impl FrameStyle {
    /// Columns the frame takes on each side, and rows it takes at the bottom. The top
    /// rule is drawn either way, so it is not counted here.
    pub fn pad(self) -> u16 {
        match self {
            FrameStyle::Full => 1,
            FrameStyle::Top => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClockStyle {
    Block,
    Plain,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SensorsConfig {
    /// Number of columns in the temperature grid.
    pub columns: usize,
    /// The most that are shown; the rest become "+N more".
    pub max: usize,
    /// Hide anything reporting exactly 0.0°C, which is what an unconnected sensor header reports.
    pub hide_zero: bool,
    /// The fallback for sensors with no threshold in sysfs.
    pub warn: f32,
    pub crit: f32,
    /// What gets a sparkline.
    ///
    /// A **key** picks exactly that reading. A **group name** picks only the first
    /// reading in the group - the row that carries the heading - so a group of three
    /// gets one trend, not three. Watching every row of a group is what turned the
    /// pane into a wall of blocks; the trend is there for CPU and GPU only.
    pub sparkline: Vec<String>,
    /// Explicit order, labels and thresholds. Empty means the automatic scan order.
    pub entry: Vec<SensorEntry>,
    /// Overrides mapping an hwmon chip name to a group name, taking priority over the built-in table.
    ///
    /// A trailing `*` matches by prefix (`"mt7921*" = "WiFi"`).
    /// For chips the built-in table does not know, or for calling them something else.
    pub group_map: BTreeMap<String, String>,
    /// The order the groups appear in. A group not listed here goes last.
    pub group_order: Vec<String>,
    /// Overrides mapping a sensor key to the label shown for it, taking priority over
    /// the built-in table.
    ///
    /// A trailing `*` matches by prefix (`"nvme/Sensor*" = "Drive"`).
    /// The **key** stays the raw `<chip>/<sysfs label>` - it is the stable identifier
    /// the rest of the config is written against - and only the display name changes.
    pub label_map: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SensorEntry {
    /// `<hwmon name>/<temp label>`. Not a path, so it survives the per-boot renumbering.
    pub key: String,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub warn: Option<f32>,
    #[serde(default)]
    pub crit: Option<f32>,
}

impl Default for SensorsConfig {
    fn default() -> Self {
        Self {
            columns: 2,
            max: 12,
            hide_zero: true,
            warn: 75.0,
            crit: 90.0,
            sparkline: vec!["CPU".to_string(), "GPU".to_string()],
            entry: Vec::new(),
            group_map: BTreeMap::new(),
            group_order: ["CPU", "GPU", "WiFi", "NVMe", "MB"]
                .iter()
                .map(|g| g.to_string())
                .collect(),
            label_map: BTreeMap::new(),
        }
    }
}

impl SensorsConfig {
    /// Whether this reading gets a sparkline.
    /// `first_in_group` is what stops a group name from matching every row it covers.
    pub fn wants_sparkline(&self, group: &str, key: &str, first_in_group: bool) -> bool {
        self.sparkline
            .iter()
            .any(|s| s == key || (first_in_group && s == group))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioConfig {
    /// The sink name passed on writes, spelled the same way the existing Hyprland keybinds do.
    pub sink: String,
    /// The source (microphone) name passed on writes.
    pub source: String,
    /// Percent added or removed per wheel notch. One notch is one percent: the wheel
    /// is for trimming, and a click on the bar is what jumps to a value.
    pub step: u16,
    /// The ceiling, in percent, that a click or the wheel can reach.
    pub max_volume: u16,
    /// A safety net so a missed signal cannot leave the display stale. 0 disables it.
    pub poll_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SignalsConfig {
    /// Receiving this re-reads the volume immediately, and nothing else.
    pub audio: SignalName,
    /// Receiving this re-reads every source.
    pub refresh_all: SignalName,
}

/// The signals `calloop` can handle, as inherited from nix.
///
/// **Real-time signals (`SIGRTMIN+n`) are not among them.**
/// waybar uses `SIGRTMIN+10`, which is therefore out of reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignalName {
    Usr1,
    Usr2,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sink: "@DEFAULT_SINK@".to_string(),
            source: "@DEFAULT_SOURCE@".to_string(),
            step: 1,
            max_volume: 150,
            poll_ms: 10_000,
        }
    }
}

impl Default for SignalsConfig {
    fn default() -> Self {
        Self {
            audio: SignalName::Usr1,
            refresh_all: SignalName::Usr2,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PowerConfig {
    /// The command that blanks the screen. The default is upstream Hyprland's `hyprctl dispatch dpms off`.
    pub screen_off: Vec<String>,
    /// `loginctl`, which works under both systemd and elogind.
    pub suspend: Vec<String>,
    pub poweroff: Vec<String>,
    /// Only screen off may skip the confirmation, since it does no harm.
    /// **The confirmation for Suspend and Power Off cannot be skipped, not even by config.**
    pub confirm_screen_off: bool,
    /// How long to wait before running the command, in milliseconds.
    ///
    /// **Never set this to 0.** The release event of an Enter press or a click
    /// would cancel the suspend or DPMS the instant it took effect. With Hyprland's
    /// `key_press_enables_dpms` or `mouse_move_enables_dpms` on, screen off is
    /// affected the same way.
    pub delay_ms: u64,
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self {
            screen_off: vec![
                "hyprctl".into(),
                "dispatch".into(),
                "dpms".into(),
                "off".into(),
            ],
            suspend: vec!["loginctl".into(), "suspend".into()],
            poweroff: vec!["loginctl".into(), "poweroff".into()],
            confirm_screen_off: false,
            delay_ms: 1000,
        }
    }
}

/// What mpris (`playerctl`) targets.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MprisConfig {
    /// The players to target. Empty means automatic detection.
    ///
    /// List them in priority order; they reach `playerctl -p` in that order.
    /// The names are the ones `playerctl -l` prints.
    pub players: Vec<String>,
}

impl MprisConfig {
    /// The `-p a,b` arguments for `playerctl`. Empty passes nothing at all.
    pub fn player_args(&self) -> Vec<String> {
        if self.players.is_empty() {
            return Vec::new();
        }
        vec!["-p".to_string(), self.players.join(",")]
    }
}

/// Album art from mpris, drawn by Überzug++ at the left of the volume and track rows.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArtConfig {
    /// Off by default: it needs Überzug++, and on Hyprland a window rule as well.
    /// On, the volume and track rows move over for the frame even when there is no art.
    pub enabled: bool,
    /// Überzug++'s `--output`. A terminal with no image protocol leaves `wayland`.
    pub output: String,
    /// How long an `https` cover may take to download, in milliseconds.
    pub download_timeout_ms: u64,
    /// What follows `dispatch ` to put the image window over its frame, with the
    /// output `wayland`. `{x}` and `{y}` are layout coordinates and `{address}` is the
    /// window's, `0x` included.
    ///
    /// Überzug++ moves its window itself, in upstream syntax, and gets it wrong on a
    /// monitor with a fractional scale. A build whose dispatcher takes another syntax
    /// refuses it altogether, so the window stays wherever it opened.
    #[serde(rename = "move")]
    pub move_window: String,
    /// What follows `dispatch ` once the window has been moved, with `{address}` as in
    /// `move`. Empty sends nothing.
    ///
    /// For a window rule that opens image windows transparent, so the moment Überzug++
    /// shows the image in the wrong place is never seen: this is what makes it visible.
    pub show: String,
}

impl Default for ArtConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            output: "wayland".to_string(),
            download_timeout_ms: 5000,
            move_window: "movewindowpixel exact {x} {y},address:{address}".to_string(),
            show: String::new(),
        }
    }
}

impl ArtConfig {
    pub fn download_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.download_timeout_ms)
    }
}

/// The names of the external commands. A name on PATH or an absolute path both work.
///
/// For distributions that name the executables differently, and for
/// swapping in a wrapper script.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandsConfig {
    /// Reads and sets the volume. It must support `-f json`.
    pub pactl: String,
    /// Subscribes to and controls mpris.
    pub playerctl: String,
    /// Draws the album art.
    pub ueberzugpp: String,
    /// Downloads album art that is not a local file.
    pub curl: String,
    /// Says which file cmus is playing. Its mpris names no file and no art.
    pub cmus_remote: String,
    /// Takes the cover out of a song file that has one embedded.
    pub ffmpeg: String,
    /// Wireless state and signal strength.
    pub wpa_cli: String,
    /// Reads the IPv4 addresses. It must support `-j`.
    pub ip: String,
    /// How long before an external command is cut off, in milliseconds.
    ///
    /// A safety net so an unresponsive `wpa_cli` cannot freeze the whole TUI.
    pub timeout_ms: u64,
    /// How long before a power command is cut off, in milliseconds.
    ///
    /// Suspend can hold the process until resume, so this is generous.
    pub power_timeout_ms: u64,
    /// How long before Hyprland's socket is cut off, in milliseconds.
    pub hyprland_timeout_ms: u64,
}

impl Default for CommandsConfig {
    fn default() -> Self {
        Self {
            pactl: "pactl".to_string(),
            playerctl: "playerctl".to_string(),
            ueberzugpp: "ueberzugpp".to_string(),
            curl: "curl".to_string(),
            cmus_remote: "cmus-remote".to_string(),
            ffmpeg: "ffmpeg".to_string(),
            wpa_cli: "wpa_cli".to_string(),
            ip: "ip".to_string(),
            timeout_ms: 1500,
            power_timeout_ms: 20_000,
            hyprland_timeout_ms: 1000,
        }
    }
}

impl CommandsConfig {
    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.timeout_ms)
    }

    pub fn power_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.power_timeout_ms)
    }

    pub fn hyprland_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.hyprland_timeout_ms)
    }
}

/// Which network interfaces make it onto the display.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkConfig {
    /// Narrow the display to exactly these. Empty means automatic detection, filtered by `exclude`.
    ///
    /// A trailing `*` matches by prefix.
    pub interface: Vec<String>,
    /// Interfaces to exclude. A trailing `*` matches by prefix.
    pub exclude: Vec<String>,
    /// Interfaces in these operstates are hidden.
    ///
    /// **Never add `dormant`.** It is the state during wpa authentication right after boot,
    /// and hiding it makes the network rows vanish entirely, leaving no way to tell
    /// connecting from broken.
    pub hide_operstate: Vec<String>,
    /// Interfaces that sort **last**. A trailing `*` matches by prefix.
    ///
    /// This is a priority, never a filter: a match here still gets its rows, it just
    /// yields the top of the pane to the real hardware. Hiding is `exclude`'s job and
    /// stays that way - excluding `veth*` by default would only mean reverting it the
    /// moment anyone wants to see one. Somebody who counts `wg0` as real hardware takes
    /// it out of this list.
    pub r#virtual: Vec<String>,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            interface: Vec::new(),
            exclude: vec!["lo".to_string()],
            hide_operstate: ["down", "lowerlayerdown", "notpresent"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            r#virtual: ["docker*", "veth*", "br-*", "virbr*", "tun*", "tap*", "wg*"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InputConfig {
    /// Mouse capture. Turning it off restores the terminal's own text selection,
    /// at the cost of every click interaction.
    pub mouse: bool,
    /// Whether clicking a workspace cell switches to it.
    pub click_workspace: bool,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            mouse: true,
            click_workspace: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeConfig {
    pub accent: ColorName,
    pub text: ColorName,
    pub dim: ColorName,
    pub warn: ColorName,
    pub crit: ColorName,
    pub border: ColorName,
    /// The trend lines. Unset, they are drawn in `accent`.
    pub sparkline: Option<ColorName>,
    /// When false, the glyphs fall back to ASCII.
    pub nerd_font: bool,
    /// How many sides of the outer frame are drawn.
    pub frame: FrameStyle,
    /// Per-block overrides. A colour left unset takes the one above.
    pub clock: PartColors,
    pub workspaces: PartColors,
    pub audio: PartColors,
    pub media: PartColors,
    pub sensors: PartColors,
    pub network: PartColors,
    pub power: PartColors,
}

/// `[theme.<block>]`: the colours one block draws in, each falling back to `[theme]`.
///
/// There is no `border` here: the frame and its dividers are shared by every block.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PartColors {
    pub accent: Option<ColorName>,
    pub text: Option<ColorName>,
    pub dim: Option<ColorName>,
    pub warn: Option<ColorName>,
    pub crit: Option<ColorName>,
    pub sparkline: Option<ColorName>,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            accent: ColorName("cyan".into()),
            text: ColorName("gray".into()),
            dim: ColorName("darkgray".into()),
            warn: ColorName("yellow".into()),
            crit: ColorName("red".into()),
            border: ColorName("darkgray".into()),
            sparkline: None,
            nerd_font: true,
            frame: FrameStyle::Full,
            clock: PartColors::default(),
            workspaces: PartColors::default(),
            audio: PartColors::default(),
            media: PartColors::default(),
            sensors: PartColors::default(),
            network: PartColors::default(),
            power: PartColors::default(),
        }
    }
}

/// A colour name or `#rrggbb`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct ColorName(pub String);

impl ColorName {
    /// An unparsable value gives `None`; the caller falls back to the default colour.
    pub fn parse(&self) -> Option<ratatui::style::Color> {
        use ratatui::style::Color;
        let raw = self.0.trim();
        if let Some(hex) = raw.strip_prefix('#') {
            if hex.len() == 6 {
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                return Some(Color::Rgb(r, g, b));
            }
            return None;
        }
        if let Ok(index) = raw.parse::<u8>() {
            return Some(Color::Indexed(index));
        }
        Some(match raw.to_ascii_lowercase().as_str() {
            "black" => Color::Black,
            "red" => Color::Red,
            "green" => Color::Green,
            "yellow" => Color::Yellow,
            "blue" => Color::Blue,
            "magenta" => Color::Magenta,
            "cyan" => Color::Cyan,
            "gray" | "grey" => Color::Gray,
            "darkgray" | "darkgrey" => Color::DarkGray,
            "lightred" => Color::LightRed,
            "lightgreen" => Color::LightGreen,
            "lightyellow" => Color::LightYellow,
            "lightblue" => Color::LightBlue,
            "lightmagenta" => Color::LightMagenta,
            "lightcyan" => Color::LightCyan,
            "white" => Color::White,
            "reset" | "default" => Color::Reset,
            _ => return None,
        })
    }
}

impl Default for General {
    fn default() -> Self {
        Self {
            tick_ms: 1000,
            sensor_tick_ms: 5000,
            network_tick_ms: 5000,
            history_len: 60,
            layout: LayoutSetting::Auto,
        }
    }
}

impl Default for WorkspacesConfig {
    fn default() -> Self {
        Self {
            count: 8,
            switch: "workspace {id}".to_string(),
        }
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self {
            format: "%H:%M:%S".to_string(),
            // No brackets around the weekday: the date and the weekday are separate spans,
            // so nothing has to mark where one stops.
            date_format: "%Y-%m-%d".to_string(),
            weekday_format: "%a".to_string(),
            style: ClockStyle::Block,
        }
    }
}

impl Config {
    /// Read `$XDG_CONFIG_HOME/pippipit/config.toml`.
    ///
    /// Missing means defaults; malformed returns an error. This is decided once, at startup.
    pub fn load() -> anyhow::Result<Self> {
        let Some(path) = Self::path() else {
            return Ok(Self::default());
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(err) => {
                return Err(
                    anyhow::Error::new(err).context(format!("failed to read {}", path.display()))
                );
            }
        };
        Self::parse(&text).map_err(|err| err.context(format!("invalid {}", path.display())))
    }

    /// Read the file the user named. Missing is an error, never a silent fallback to defaults.
    pub fn load_from(path: &std::path::Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            anyhow::Error::new(e).context(format!("failed to read {}", path.display()))
        })?;
        Self::parse(&text).map_err(|e| e.context(format!("invalid {}", path.display())))
    }

    /// Parse from a TOML string. Used by the tests.
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(text)?)
    }

    fn path() -> Option<PathBuf> {
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
        };
        Some(base.join("pippipit").join("config.toml"))
    }
}

#[cfg(test)]
mod tests {

    /// A group of three sensors must get one trend, not three: watching every row of
    /// the GPU turned that corner of the pane into a wall of blocks.
    #[test]
    fn a_group_name_gives_the_group_a_single_sparkline() {
        let c = SensorsConfig::default();
        assert!(
            c.wants_sparkline("GPU", "amdgpu/edge", true),
            "the heading row"
        );
        assert!(!c.wants_sparkline("GPU", "amdgpu/junction", false));
        assert!(!c.wants_sparkline("GPU", "amdgpu/mem", false));
        // A group nobody asked for gets nothing either way.
        assert!(!c.wants_sparkline("MB", "asusec/VRM", true));
    }

    /// A key pins the trend to one row wherever it sits in its group.
    #[test]
    fn a_key_pins_the_sparkline_to_one_row() {
        let c = SensorsConfig {
            sparkline: vec!["amdgpu/junction".into()],
            ..SensorsConfig::default()
        };
        assert!(c.wants_sparkline("GPU", "amdgpu/junction", false));
        assert!(!c.wants_sparkline("GPU", "amdgpu/edge", true));
    }

    /// The shipped example must actually parse: `deny_unknown_fields` means a stale
    /// key in it is a startup failure for anyone who copies it.
    #[test]
    fn the_example_config_parses() {
        let text = include_str!("../config.example.toml");
        Config::parse(text).expect("config.example.toml must parse");
    }

    /// A block's colours sit under `[theme.<block>]`, and a typo in one is as much an
    /// error as anywhere else.
    #[test]
    fn a_block_colour_parses_and_a_typo_in_it_does_not() {
        let c = Config::parse("[theme.clock]\naccent = \"#ebcb8b\"\n").unwrap();
        assert_eq!(c.theme.clock.accent, Some(ColorName("#ebcb8b".into())));
        assert_eq!(c.theme.audio.accent, None);
        assert!(Config::parse("[theme.clock]\naccnet = \"red\"\n").is_err());
        assert!(Config::parse("[theme.clock]\nborder = \"red\"\n").is_err());
        assert!(Config::parse("[theme.clok]\naccent = \"red\"\n").is_err());
    }

    /// `virtual` is a Rust keyword, so the field is a raw identifier. Serde still has
    /// to see it under its plain name, or the key silently fails `deny_unknown_fields`.
    #[test]
    fn the_virtual_key_parses_under_its_plain_name() {
        let c = Config::parse("[network]\nvirtual = [\"podman*\"]\n").unwrap();
        assert_eq!(c.network.r#virtual, vec!["podman*".to_string()]);
        // The other keys keep their defaults.
        assert_eq!(c.network.exclude, vec!["lo".to_string()]);
    }
    use super::MprisConfig;

    /// With nothing set, no `-p` is passed and playerctl detects on its own.
    #[test]
    fn no_players_means_no_player_args() {
        assert!(MprisConfig::default().player_args().is_empty());
    }

    /// The listed order is the priority order.
    #[test]
    fn players_become_one_comma_separated_arg() {
        let c = MprisConfig {
            players: vec!["mpd".into(), "firefox".into()],
        };
        assert_eq!(c.player_args(), ["-p", "mpd,firefox"]);
    }

    use super::*;

    /// The repository's `config.example.toml` **must actually parse**.
    ///
    /// With `deny_unknown_fields`, adding or removing a setting without updating the example fails here.
    /// That is what stops the docs drifting from the code.
    #[test]
    fn example_config_parses() {
        let text = include_str!("../config.example.toml");
        Config::parse(text).expect("config.example.toml must parse");
    }

    /// The values in the example must match the defaults.
    /// The example promises that copying it whole changes nothing, so this has to hold.
    #[test]
    fn example_config_matches_the_defaults() {
        let text = include_str!("../config.example.toml");
        let parsed = Config::parse(text).unwrap();
        let default = Config::default();

        assert_eq!(parsed.general.tick_ms, default.general.tick_ms);
        assert_eq!(
            parsed.general.sensor_tick_ms,
            default.general.sensor_tick_ms
        );
        assert_eq!(
            parsed.general.network_tick_ms,
            default.general.network_tick_ms
        );
        assert_eq!(parsed.general.history_len, default.general.history_len);
        assert_eq!(parsed.clock.style, default.clock.style);
        assert_eq!(parsed.input.mouse, default.input.mouse);
        assert_eq!(parsed.audio.sink, default.audio.sink);
        assert_eq!(parsed.audio.step, default.audio.step);
        assert_eq!(parsed.audio.poll_ms, default.audio.poll_ms);
        assert_eq!(parsed.sensors.columns, default.sensors.columns);
        assert_eq!(parsed.sensors.hide_zero, default.sensors.hide_zero);
        assert_eq!(parsed.sensors.sparkline, default.sensors.sparkline);
        assert_eq!(parsed.sensors.group_order, default.sensors.group_order);
        assert_eq!(parsed.sensors.group_map, default.sensors.group_map);
        assert_eq!(parsed.power.screen_off, default.power.screen_off);
        assert_eq!(parsed.power.suspend, default.power.suspend);
        assert_eq!(parsed.power.poweroff, default.power.poweroff);
        assert_eq!(parsed.power.delay_ms, default.power.delay_ms);
        assert_eq!(parsed.mpris.players, default.mpris.players);
        assert_eq!(parsed.art.enabled, default.art.enabled);
        assert_eq!(parsed.art.output, default.art.output);
        assert_eq!(
            parsed.art.download_timeout_ms,
            default.art.download_timeout_ms
        );
        assert_eq!(parsed.art.move_window, default.art.move_window);
        assert_eq!(parsed.art.show, default.art.show);
        assert_eq!(parsed.commands.ueberzugpp, default.commands.ueberzugpp);
        assert_eq!(parsed.commands.curl, default.commands.curl);
        assert_eq!(parsed.commands.cmus_remote, default.commands.cmus_remote);
        assert_eq!(parsed.commands.ffmpeg, default.commands.ffmpeg);
        assert_eq!(parsed.network.interface, default.network.interface);
        assert_eq!(parsed.network.exclude, default.network.exclude);
        assert_eq!(
            parsed.network.hide_operstate,
            default.network.hide_operstate
        );
        assert_eq!(parsed.commands.pactl, default.commands.pactl);
        assert_eq!(parsed.commands.playerctl, default.commands.playerctl);
        assert_eq!(parsed.commands.wpa_cli, default.commands.wpa_cli);
        assert_eq!(parsed.commands.ip, default.commands.ip);
        assert_eq!(parsed.commands.timeout_ms, default.commands.timeout_ms);
        assert_eq!(
            parsed.commands.power_timeout_ms,
            default.commands.power_timeout_ms
        );
        assert_eq!(
            parsed.commands.hyprland_timeout_ms,
            default.commands.hyprland_timeout_ms
        );
        assert_eq!(parsed.theme.accent, default.theme.accent);
        assert_eq!(parsed.signals.audio, default.signals.audio);
    }

    #[test]
    fn layout_accepts_every_documented_value() {
        for value in ["auto", "2-pane", "2-pane-narrow", "1-pane", "bar"] {
            let text = format!("[general]\nlayout = \"{value}\"\n");
            Config::parse(&text).unwrap_or_else(|e| panic!("layout={value}: {e}"));
        }
        assert!(Config::parse("[general]\nlayout = \"nope\"\n").is_err());
    }

    #[test]
    fn empty_config_is_all_defaults() {
        let parsed = Config::parse("").unwrap();
        assert_eq!(parsed.general.tick_ms, Config::default().general.tick_ms);
    }

    /// A typo is never silently ignored.
    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::parse("[general]\ntikc_ms = 500\n").is_err());
        assert!(Config::parse("[nosuchsection]\nx = 1\n").is_err());
    }

    #[test]
    fn partial_config_keeps_other_defaults() {
        let parsed = Config::parse("[general]\ntick_ms = 250\n").unwrap();
        assert_eq!(parsed.general.tick_ms, 250);
        assert_eq!(
            parsed.general.history_len,
            Config::default().general.history_len
        );
        assert_eq!(parsed.power.delay_ms, Config::default().power.delay_ms);
    }

    /// A zero power delay causes accidents, but the config is still respected.
    /// At least the default must not be 0 (also covered by a test in `power.rs`).
    #[test]
    fn power_delay_is_configurable_but_defaults_to_one_second() {
        assert_eq!(Config::default().power.delay_ms, 1000);
        let parsed = Config::parse("[power]\ndelay_ms = 0\n").unwrap();
        assert_eq!(parsed.power.delay_ms, 0);
    }
}
