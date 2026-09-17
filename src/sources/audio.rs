//! Volume, read from `pactl -f json`.
//!
//! The text output shifts with locale and channel layout, so it is never parsed.
//! Writes (volume, mute) use the same commands as the existing Hyprland keybinds.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::config::{AudioConfig, CommandsConfig};
use crate::sources::provider::{
    COALESCE_WINDOW, Coalesce, CommandRunner, Fold, ProviderHandle, Shutdown, SystemRunner,
    wait_for_commands,
};

/// What one device needs to contribute to the display.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    /// The name shown in the UI: `description`, not the sprawling ALSA `name`.
    pub description: String,
    /// Roughly 0..=150. When channels differ, the maximum wins.
    pub volume: u16,
    pub muted: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Audio {
    pub sink: Option<Device>,
    pub source: Option<Device>,
}

// ---- pactl's JSON ----

#[derive(Debug, Deserialize)]
struct PaInfo {
    #[serde(default)]
    default_sink_name: String,
    #[serde(default)]
    default_source_name: String,
}

#[derive(Debug, Deserialize)]
struct PaDevice {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    mute: bool,
    /// Channel name to volume. A BTreeMap keeps the order stable.
    #[serde(default)]
    volume: BTreeMap<String, PaChannel>,
}

#[derive(Debug, Deserialize)]
struct PaChannel {
    /// Arrives in the form `"35%"`.
    #[serde(default)]
    value_percent: String,
}

/// `"35%"` becomes `35`; malformed input gives `None`.
fn parse_percent(text: &str) -> Option<u16> {
    text.trim().trim_end_matches('%').trim().parse().ok()
}

/// When channels disagree, take the **maximum**.
/// Otherwise one turned-down channel would read as a low overall volume.
fn device_volume(device: &PaDevice) -> u16 {
    device
        .volume
        .values()
        .filter_map(|c| parse_percent(&c.value_percent))
        .max()
        .unwrap_or(0)
}

fn pick<'a>(devices: &'a [PaDevice], wanted: &str) -> Option<&'a PaDevice> {
    devices.iter().find(|d| d.name == wanted)
}

fn to_device(device: &PaDevice) -> Device {
    Device {
        description: device
            .description
            .clone()
            .unwrap_or_else(|| device.name.clone()),
        volume: device_volume(device),
        muted: device.mute,
    }
}

/// Pick the default sink and source out of the three JSON documents.
///
/// Parsing is split out as a pure function, so fixtures can test it.
pub fn parse(info_json: &str, sinks_json: &str, sources_json: &str) -> Result<Audio, String> {
    let info: PaInfo = serde_json::from_str(info_json).map_err(|e| format!("info: {e}"))?;
    let sinks: Vec<PaDevice> =
        serde_json::from_str(sinks_json).map_err(|e| format!("sinks: {e}"))?;
    let sources: Vec<PaDevice> =
        serde_json::from_str(sources_json).map_err(|e| format!("sources: {e}"))?;

    Ok(Audio {
        sink: pick(&sinks, &info.default_sink_name).map(to_device),
        source: pick(&sources, &info.default_source_name).map(to_device),
    })
}

fn pactl(
    commands: &CommandsConfig,
    runner: &dyn CommandRunner,
    args: &[&str],
) -> Result<String, String> {
    runner.output(&commands.pactl, args)
}

pub fn read(commands: &CommandsConfig, runner: &dyn CommandRunner) -> Result<Audio, String> {
    let info = pactl(commands, runner, &["-f", "json", "info"])?;
    let sinks = pactl(commands, runner, &["-f", "json", "list", "sinks"])?;
    let sources = pactl(commands, runner, &["-f", "json", "list", "sources"])?;
    parse(&info, &sinks, &sources)
}

// ---- Writes ----

/// Which device a write is aimed at.
///
/// Output and input are drawn the same way, so **every action has to say which one
/// it means**: a symmetrical display whose controls are not is the worst of both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Sink,
    Source,
}

impl Target {
    /// The `pactl` verb, `set-sink-…` or `set-source-…`.
    fn verb(self, action: &str) -> String {
        match self {
            Target::Sink => format!("set-sink-{action}"),
            Target::Source => format!("set-source-{action}"),
        }
    }

    /// The device name to pass, as configured.
    fn device(self, config: &AudioConfig) -> &str {
        match self {
            Target::Sink => &config.sink,
            Target::Source => &config.source,
        }
    }
}

/// Set the volume to an absolute value.
pub fn set_volume(
    target: Target,
    config: &AudioConfig,
    commands: &CommandsConfig,
    runner: &dyn CommandRunner,
    percent: u16,
) -> Result<(), String> {
    let percent = percent.min(config.max_volume);
    pactl(
        commands,
        runner,
        &[
            &target.verb("volume"),
            target.device(config),
            &format!("{percent}%"),
        ],
    )
    .map(|_| ())
}

/// Move the volume relative to where it is.
pub fn nudge_volume(
    target: Target,
    config: &AudioConfig,
    commands: &CommandsConfig,
    runner: &dyn CommandRunner,
    delta: i16,
) -> Result<(), String> {
    let arg = if delta >= 0 {
        format!("+{delta}%")
    } else {
        format!("{delta}%")
    };
    pactl(
        commands,
        runner,
        &[&target.verb("volume"), target.device(config), &arg],
    )
    .map(|_| ())
}

pub fn toggle_mute(
    target: Target,
    config: &AudioConfig,
    commands: &CommandsConfig,
    runner: &dyn CommandRunner,
) -> Result<(), String> {
    pactl(
        commands,
        runner,
        &[&target.verb("mute"), target.device(config), "toggle"],
    )
    .map(|_| ())
}

// ---- Provider ----

/// One sample from the audio provider.
#[derive(Debug, Clone)]
pub enum AudioSample {
    /// A read finished, or failed.
    State(Result<Audio, String>),
    /// A write failed. The values on screen are still the last good ones.
    CommandFailed(String),
}

/// What can be asked of the audio provider.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum AudioCommand {
    /// Move the volume by a step, the way a wheel notch does.
    Nudge(Target, i16),
    /// Put the volume at a value.
    Set(Target, u16),
    ToggleMute(Target),
    Refresh,
}

/// What two commands must agree on before they can be folded together.
#[derive(PartialEq)]
pub enum AudioKey {
    Volume(Target),
    Mute(Target),
    Refresh,
}

impl Coalesce for AudioCommand {
    type Key = AudioKey;

    fn key(&self) -> AudioKey {
        match self {
            AudioCommand::Nudge(target, _) | AudioCommand::Set(target, _) => {
                AudioKey::Volume(*target)
            }
            AudioCommand::ToggleMute(target) => AudioKey::Mute(*target),
            AudioCommand::Refresh => AudioKey::Refresh,
        }
    }

    fn fold(&mut self, next: &Self) -> Fold {
        match (&*self, next) {
            // Ten notches of the wheel are one write of the sum.
            (AudioCommand::Nudge(target, so_far), AudioCommand::Nudge(_, step)) => {
                *self = AudioCommand::Nudge(*target, so_far.saturating_add(*step));
                Fold::Merged
            }
            // A step after a value moves that value, not what is on the device.
            (AudioCommand::Set(target, value), AudioCommand::Nudge(_, step)) => {
                *self = AudioCommand::Set(*target, value.saturating_add_signed(*step));
                Fold::Merged
            }
            // A value discards whatever was accumulating before it.
            (_, AudioCommand::Set(..)) => {
                *self = *next;
                Fold::Merged
            }
            (AudioCommand::ToggleMute(_), AudioCommand::ToggleMute(_)) => Fold::Cancelled,
            (AudioCommand::Refresh, AudioCommand::Refresh) => Fold::Merged,
            _ => Fold::Keep,
        }
    }

    /// `pactl` writes to one device at a time, so output and input never depend on the
    /// order they are run in. A read has to stay where it is: it reports what the
    /// writes before it did.
    fn commutes_with(&self, next: &Self) -> bool {
        match (self.target(), next.target()) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        }
    }
}

impl AudioCommand {
    fn target(&self) -> Option<Target> {
        match self {
            AudioCommand::Nudge(target, _)
            | AudioCommand::Set(target, _)
            | AudioCommand::ToggleMute(target) => Some(*target),
            AudioCommand::Refresh => None,
        }
    }
}

/// Talks to `pactl` on its own thread: writes first, then one read for all of them.
pub struct AudioProvider {
    pub config: AudioConfig,
    pub commands: CommandsConfig,
    /// The safety poll. `None` when it is turned off and only signals and clicks arrive.
    pub tick: Option<Duration>,
}

impl AudioProvider {
    pub fn spawn(
        self,
        samples: calloop::channel::Sender<AudioSample>,
    ) -> Result<ProviderHandle<AudioCommand>, String> {
        self.spawn_with(samples, None)
    }

    /// `runner` stands in for `pactl`, which is what lets a test drive this without one.
    fn spawn_with(
        self,
        samples: calloop::channel::Sender<AudioSample>,
        runner: Option<Arc<dyn CommandRunner>>,
    ) -> Result<ProviderHandle<AudioCommand>, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let shutdown = Shutdown::new();
        let stop = shutdown.clone();
        let join = std::thread::Builder::new()
            .name("pippipit-audio".into())
            .spawn(move || {
                let runner: Arc<dyn CommandRunner> = runner.unwrap_or_else(|| {
                    Arc::new(SystemRunner::new(self.commands.timeout(), stop.clone()))
                });
                let runner = runner.as_ref();
                loop {
                    let state = read(&self.commands, runner);
                    if samples.send(AudioSample::State(state)).is_err() {
                        return; // the event loop is gone
                    }
                    // Without a poll there is still a wait, so a stop is noticed promptly.
                    let deadline = Instant::now() + self.tick.unwrap_or(IDLE_WAIT);
                    let Some(commands) = wait_for_commands(&rx, &stop, deadline, COALESCE_WINDOW)
                    else {
                        return;
                    };
                    for command in commands {
                        if let Err(message) = self.apply(command, runner)
                            && samples.send(AudioSample::CommandFailed(message)).is_err()
                        {
                            return;
                        }
                    }
                }
            })
            .map_err(|e| format!("failed to spawn the audio thread: {e}"))?;
        Ok(ProviderHandle::new(tx, shutdown, join))
    }

    fn apply(&self, command: AudioCommand, runner: &dyn CommandRunner) -> Result<(), String> {
        match command {
            AudioCommand::Nudge(target, delta) => {
                nudge_volume(target, &self.config, &self.commands, runner, delta)
            }
            AudioCommand::Set(target, percent) => {
                set_volume(target, &self.config, &self.commands, runner, percent)
            }
            AudioCommand::ToggleMute(target) => {
                toggle_mute(target, &self.config, &self.commands, runner)
            }
            AudioCommand::Refresh => Ok(()),
        }
    }
}

/// How long the provider waits between reads when the safety poll is off.
const IDLE_WAIT: Duration = Duration::from_secs(3600);

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;
    use std::time::Instant;

    /// Stands in for `pactl`, answering reads from the fixtures and recording every call.
    struct FakePactl {
        calls: Mutex<Vec<String>>,
    }

    impl FakePactl {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
            })
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn writes(&self) -> Vec<String> {
            self.calls()
                .into_iter()
                .filter(|call| call.contains("set-sink") || call.contains("set-source"))
                .collect()
        }

        fn reads(&self) -> usize {
            self.calls()
                .iter()
                .filter(|call| call.contains("json info"))
                .count()
        }
    }

    impl CommandRunner for FakePactl {
        fn output(&self, _program: &str, args: &[&str]) -> Result<String, String> {
            self.calls.lock().unwrap().push(args.join(" "));
            match args {
                [.., "info"] => Ok(INFO.to_string()),
                [.., "sinks"] => Ok(SINKS.to_string()),
                [.., "sources"] => Ok(SOURCES.to_string()),
                _ => Ok(String::new()),
            }
        }
    }

    /// Ten notches of the wheel must reach `pactl` as one write, followed by one read.
    ///
    /// Sent straight through, each notch would cost a write and three reads, all of
    /// them in front of the next keystroke.
    #[test]
    fn a_burst_of_wheel_notches_becomes_one_write_and_one_read() {
        let fake = FakePactl::new();
        let mut collected: Vec<AudioSample> = Vec::new();
        let mut event_loop = calloop::EventLoop::<Vec<AudioSample>>::try_new().unwrap();
        let (samples, rx) = calloop::channel::channel();
        event_loop
            .handle()
            .insert_source(rx, |message, _, out: &mut Vec<AudioSample>| {
                if let calloop::channel::Event::Msg(sample) = message {
                    out.push(sample);
                }
            })
            .unwrap();

        let provider = AudioProvider {
            config: AudioConfig::default(),
            commands: CommandsConfig::default(),
            tick: None,
        }
        .spawn_with(samples, Some(fake.clone()))
        .unwrap();

        // Wait for the read the provider opens with, so the burst lands on a settled thread.
        let deadline = Instant::now() + Duration::from_secs(5);
        while collected.is_empty() && Instant::now() < deadline {
            event_loop
                .dispatch(Some(Duration::from_millis(50)), &mut collected)
                .unwrap();
        }
        assert_eq!(fake.reads(), 1, "one read at startup");

        let wheel = provider.sender();
        for _ in 0..10 {
            wheel.send(AudioCommand::Nudge(Target::Sink, 5)).unwrap();
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        while fake.reads() < 2 && Instant::now() < deadline {
            event_loop
                .dispatch(Some(Duration::from_millis(50)), &mut collected)
                .unwrap();
        }

        assert_eq!(
            fake.writes(),
            vec!["set-sink-volume @DEFAULT_SINK@ +50%"],
            "ten notches, one write, the sum of them"
        );
        assert_eq!(
            fake.reads(),
            2,
            "one read after the burst, not one per notch"
        );
    }

    /// A write that fails is reported without throwing away what is on screen.
    #[test]
    fn a_failed_write_comes_back_as_a_sample() {
        struct Failing;
        impl CommandRunner for Failing {
            fn output(&self, _program: &str, args: &[&str]) -> Result<String, String> {
                match args {
                    [.., "info"] => Ok(INFO.to_string()),
                    [.., "sinks"] => Ok(SINKS.to_string()),
                    [.., "sources"] => Ok(SOURCES.to_string()),
                    _ => Err("pactl: no such sink".to_string()),
                }
            }
        }

        let mut collected: Vec<AudioSample> = Vec::new();
        let mut event_loop = calloop::EventLoop::<Vec<AudioSample>>::try_new().unwrap();
        let (samples, rx) = calloop::channel::channel();
        event_loop
            .handle()
            .insert_source(rx, |message, _, out: &mut Vec<AudioSample>| {
                if let calloop::channel::Event::Msg(sample) = message {
                    out.push(sample);
                }
            })
            .unwrap();

        let provider = AudioProvider {
            config: AudioConfig::default(),
            commands: CommandsConfig::default(),
            tick: None,
        }
        .spawn_with(samples, Some(Arc::new(Failing)))
        .unwrap();

        provider
            .sender()
            .send(AudioCommand::ToggleMute(Target::Sink))
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let failed = |seen: &[AudioSample]| {
            seen.iter()
                .any(|sample| matches!(sample, AudioSample::CommandFailed(_)))
        };
        while !failed(&collected) && Instant::now() < deadline {
            event_loop
                .dispatch(Some(Duration::from_millis(50)), &mut collected)
                .unwrap();
        }
        assert!(failed(&collected), "the failure has to reach the state");
        assert!(
            collected
                .iter()
                .any(|sample| matches!(sample, AudioSample::State(Ok(_)))),
            "and the last good values still arrived"
        );
    }

    const INFO: &str = include_str!("../../tests/fixtures/pactl-info.json");
    const SINKS: &str = include_str!("../../tests/fixtures/pactl-sinks.json");
    const SOURCES: &str = include_str!("../../tests/fixtures/pactl-sources.json");

    /// The fixture has four sinks; `default_sink_name` must pick exactly one.
    #[test]
    fn picks_the_default_sink_out_of_several() {
        let audio = parse(INFO, SINKS, SOURCES).expect("parse");
        let sink = audio.sink.expect("sink");
        assert_eq!(sink.description, "USB Audio Speakers");
        assert_eq!(sink.volume, 35);
        assert!(!sink.muted);
    }

    /// The source side must likewise resolve to the one named by `default_source_name`.
    /// The default device varies per system, so the description is never hard-coded.
    #[test]
    fn picks_the_default_source() {
        let info: PaInfo = serde_json::from_str(INFO).unwrap();
        let sources: Vec<PaDevice> = serde_json::from_str(SOURCES).unwrap();
        let expected = sources
            .iter()
            .find(|d| d.name == info.default_source_name)
            .map(to_device)
            .expect("fixture should contain the default source");

        let audio = parse(INFO, SINKS, SOURCES).expect("parse");
        assert_eq!(audio.source, Some(expected));
    }

    #[test]
    fn parse_percent_handles_the_pactl_format() {
        assert_eq!(parse_percent("35%"), Some(35));
        assert_eq!(parse_percent(" 100% "), Some(100));
        assert_eq!(parse_percent("153%"), Some(153));
        assert_eq!(parse_percent(""), None);
        assert_eq!(parse_percent("n/a"), None);
    }

    /// When left and right differ, the maximum wins.
    #[test]
    fn takes_the_max_across_channels() {
        let json = r#"[{"name":"s","description":"S","mute":false,"volume":{
            "front-left":{"value_percent":"20%"},
            "front-right":{"value_percent":"80%"}}}]"#;
        let info = r#"{"default_sink_name":"s","default_source_name":""}"#;
        let audio = parse(info, json, "[]").unwrap();
        assert_eq!(audio.sink.unwrap().volume, 80);
    }

    /// A default that is missing from the list (just after a hot-swap, say) must not panic.
    #[test]
    fn missing_default_yields_none() {
        let info = r#"{"default_sink_name":"nope","default_source_name":"nope"}"#;
        let audio = parse(info, SINKS, SOURCES).unwrap();
        assert!(audio.sink.is_none());
        assert!(audio.source.is_none());
    }

    /// Without a description, it falls back to the name.
    #[test]
    fn falls_back_to_name_without_description() {
        let json = r#"[{"name":"raw_name","mute":true,"volume":{}}]"#;
        let info = r#"{"default_sink_name":"raw_name","default_source_name":""}"#;
        let sink = parse(info, json, "[]").unwrap().sink.unwrap();
        assert_eq!(sink.description, "raw_name");
        assert_eq!(sink.volume, 0);
        assert!(sink.muted);
    }

    #[test]
    fn broken_json_is_an_error_not_a_panic() {
        assert!(parse("{", "[]", "[]").is_err());
        assert!(parse(r#"{"default_sink_name":"x"}"#, "not json", "[]").is_err());
    }
}
