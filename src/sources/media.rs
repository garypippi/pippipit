//! mpris. A resident `playerctl -F` pushes updates in.
//!
//! **With no player at all, `playerctl -F` prints nothing and waits** (measured).
//! So the lines alone cannot tell "nothing changed yet" from "no player is running".
//! A liveness check runs `playerctl -l` at startup and periodically.

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::{CommandsConfig, MprisConfig};
use crate::sources::provider::{
    COALESCE_WINDOW, Coalesce, CommandRunner, Fold, ProviderHandle, Shutdown, SystemRunner,
    wait_for_commands,
};
use crate::util::proc;

/// The field separator. A tab in a track title is effectively unheard of.
const SEP: char = '\t';

/// Free-form fields go last, so the fixed ones never get a separator mixed in. The art
/// URL counts as fixed: a URL carries no raw tab.
const FORMAT: &str = concat!(
    "{{playerName}}\t{{status}}\t{{position}}\t{{mpris:length}}\t{{mpris:artUrl}}\t",
    "{{title}}\t{{artist}}\t{{album}}"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Playing,
    Paused,
    Stopped,
}

impl Status {
    fn parse(text: &str) -> Self {
        match text.trim() {
            "Playing" => Status::Playing,
            "Stopped" => Status::Stopped,
            _ => Status::Paused,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    pub player: String,
    pub status: Status,
    /// Microseconds, the unit `playerctl` reports in.
    pub position_us: u64,
    pub length_us: u64,
    /// `mpris:artUrl`. Empty when the player gives none.
    pub art_url: String,
    pub title: String,
    pub artist: String,
    pub album: String,
}

/// Parse one line of `-F` output.
///
/// A malformed line is dropped (`None`). This is a resident TUI; it does not die over one.
pub fn parse_line(line: &str) -> Option<Media> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim().is_empty() {
        return None;
    }
    let mut fields = line.splitn(8, SEP);
    let player = fields.next()?.to_string();
    let status = Status::parse(fields.next()?);
    let position_us = fields.next()?.trim().parse().unwrap_or(0);
    let length_us = fields.next()?.trim().parse().unwrap_or(0);
    let art_url = fields.next().unwrap_or("").trim().to_string();
    let title = fields.next().unwrap_or("").to_string();
    let artist = fields.next().unwrap_or("").to_string();
    let album = fields.next().unwrap_or("").to_string();

    if player.is_empty() {
        return None;
    }
    Some(Media {
        player,
        status,
        position_us,
        length_us,
        art_url,
        title,
        artist,
        album,
    })
}

/// Whether any targeted player is present.
///
/// With no `players` set, that is `playerctl -l` (exit 1 and `No players found` when absent).
/// When it is set, `-l` would list every player regardless, so the check becomes
/// **whether `status` succeeds with `-p`**.
pub fn any_player(
    commands: &CommandsConfig,
    mpris: &MprisConfig,
    runner: &dyn CommandRunner,
) -> bool {
    let players = mpris.player_args();
    if players.is_empty() {
        return runner
            .output(&commands.playerctl, &["-l"])
            .map(|out| !out.trim().is_empty())
            .unwrap_or(false);
    }
    let mut args: Vec<&str> = players.iter().map(String::as_str).collect();
    args.push("status");
    runner.output(&commands.playerctl, &args).is_ok()
}

/// Spawn `playerctl -F`.
pub fn spawn_follow(commands: &CommandsConfig, mpris: &MprisConfig) -> std::io::Result<Child> {
    let mut command = Command::new(&commands.playerctl);
    command
        .args(mpris.player_args())
        .args(["-F", "metadata", "--format", FORMAT])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        // Its own group, so a stop takes anything it started with it.
        .process_group(0);
    // It says nothing until the track changes, so a dead reader goes unnoticed: without
    // this it outlives a panel that was killed.
    proc::die_with_parent(&mut command).spawn()
}

// ---- Controls ----

fn control(
    commands: &CommandsConfig,
    mpris: &MprisConfig,
    runner: &dyn CommandRunner,
    args: &[&str],
) -> Result<(), String> {
    let players = mpris.player_args();
    let mut argv: Vec<&str> = players.iter().map(String::as_str).collect();
    argv.extend_from_slice(args);
    runner.status(&commands.playerctl, &argv)
}

pub fn play_pause(
    commands: &CommandsConfig,
    mpris: &MprisConfig,
    runner: &dyn CommandRunner,
) -> Result<(), String> {
    control(commands, mpris, runner, &["play-pause"])
}

pub fn previous(
    commands: &CommandsConfig,
    mpris: &MprisConfig,
    runner: &dyn CommandRunner,
) -> Result<(), String> {
    control(commands, mpris, runner, &["previous"])
}

pub fn next(
    commands: &CommandsConfig,
    mpris: &MprisConfig,
    runner: &dyn CommandRunner,
) -> Result<(), String> {
    control(commands, mpris, runner, &["next"])
}

/// Set the position, in seconds.
pub fn seek_to(
    commands: &CommandsConfig,
    mpris: &MprisConfig,
    runner: &dyn CommandRunner,
    seconds: f64,
) -> Result<(), String> {
    control(
        commands,
        mpris,
        runner,
        &["position", &format!("{seconds:.0}")],
    )
}

// ---- Provider ----

/// One sample from the media provider. `None` means no player is present.
#[derive(Debug, Clone)]
pub enum MediaSample {
    State(Option<Media>),
    Failed(String),
}

/// What reaches the media coordinator.
///
/// The first four come from the panel. The last two come from the reader thread, which
/// is the only thing that touches the resident `playerctl -F`.
#[derive(Debug, Clone, PartialEq)]
pub enum MediaCommand {
    PlayPause,
    Previous,
    Next,
    SeekTo(f64),
    Line(Option<Media>),
    Disconnected(String),
}

#[derive(PartialEq)]
pub enum MediaKey {
    Control,
    Line,
    Disconnected,
}

impl Coalesce for MediaCommand {
    type Key = MediaKey;

    fn key(&self) -> MediaKey {
        match self {
            MediaCommand::Line(_) => MediaKey::Line,
            MediaCommand::Disconnected(_) => MediaKey::Disconnected,
            _ => MediaKey::Control,
        }
    }

    fn fold(&mut self, next: &Self) -> Fold {
        match (&*self, next) {
            // Only the newest metadata line is worth drawing.
            (MediaCommand::Line(_), MediaCommand::Line(_)) => {
                *self = next.clone();
                Fold::Merged
            }
            (MediaCommand::Disconnected(_), MediaCommand::Disconnected(_)) => Fold::Merged,
            // Two presses of play are two presses, not one.
            _ => Fold::Keep,
        }
    }
}

/// Follows `playerctl -F` on a reader thread and runs the transport controls itself.
pub struct MediaProvider {
    pub commands: CommandsConfig,
    pub mpris: MprisConfig,
    /// How often to check that a player is still there. `-F` cannot report it going away.
    pub liveness_tick: Duration,
}

impl MediaProvider {
    pub fn spawn(
        self,
        samples: calloop::channel::Sender<MediaSample>,
    ) -> Result<ProviderHandle<MediaCommand>, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let shutdown = Shutdown::new();

        // The resident `playerctl -F` lives on the reader thread, along with the
        // reconnecting. Keeping it off the coordinator is what lets a button press be
        // answered while the reader is between connections - waiting out a backoff of
        // up to half a minute with a press queued behind it is not a working panel.
        let child: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
        let reader_stop = shutdown.clone();
        let reader_child = Arc::clone(&child);
        let lines = tx.clone();
        let reader_commands = self.commands.clone();
        let reader_mpris = self.mpris.clone();
        let reader = std::thread::Builder::new()
            .name("pippipit-media-rd".into())
            .spawn(move || {
                let mut backoff = Duration::from_secs(1);
                while !reader_stop.is_requested() {
                    let spoke = follow_once(
                        &reader_commands,
                        &reader_mpris,
                        &reader_child,
                        &lines,
                        &reader_stop,
                    );
                    // The player was there and said something, so start the wait over.
                    if spoke {
                        backoff = Duration::from_secs(1);
                    }
                    if !reader_stop.sleep(backoff) {
                        return;
                    }
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            })
            .map_err(|e| format!("failed to spawn the media reader: {e}"))?;

        // Held where both the coordinator and the failure path below can reach it, for
        // the same reason the child is: whoever gets it first is the one who ends it.
        let reader = Arc::new(Mutex::new(Some(reader)));
        let reader_for_coordinator = Arc::clone(&reader);

        let stop = shutdown.clone();
        let child_for_cleanup = Arc::clone(&child);
        let join = std::thread::Builder::new()
            .name("pippipit-media".into())
            .spawn(move || {
                let runner = SystemRunner::new(self.commands.timeout(), stop.clone());
                let mut playing: Option<Media> = None;
                loop {
                    let deadline = Instant::now() + self.liveness_tick;
                    let Some(commands) = wait_for_commands(&rx, &stop, deadline, COALESCE_WINDOW)
                    else {
                        break;
                    };
                    // An empty burst is the liveness check coming due.
                    if commands.is_empty() {
                        if playing.is_some() && !any_player(&self.commands, &self.mpris, &runner) {
                            playing = None;
                            if samples.send(MediaSample::State(None)).is_err() {
                                break;
                            }
                        }
                        continue;
                    }
                    let mut gone = false;
                    for command in commands {
                        match command {
                            MediaCommand::Line(media) => {
                                playing = media.clone();
                                if samples.send(MediaSample::State(media)).is_err() {
                                    gone = true;
                                }
                            }
                            MediaCommand::Disconnected(_) => {
                                // The player is gone, so the track goes with it. A
                                // reconnect brings it back; leaving it on screen would
                                // be a lie.
                                playing = None;
                                let _ = samples.send(MediaSample::State(None));
                            }
                            control => {
                                if let Err(message) = self.control(control, &runner) {
                                    let _ = samples.send(MediaSample::Failed(message));
                                }
                            }
                        }
                    }
                    if gone {
                        break; // the event loop is gone
                    }
                }
                // Both ways out of the loop come through here. Telling the reader to
                // stop before waiting for it matters on the way out that the event loop
                // starts: without it the reader takes the killed player for a
                // disconnect and starts another one, and this wait never ends.
                stop.request();
                // It is blocked on a pipe, which has no read timeout: killing what it
                // is reading from is what ends it.
                end_follower(&child);
                join_reader(&reader_for_coordinator);
            });
        // The reader is already running, so a coordinator that never starts has to be
        // cleaned up after by hand: nothing else holds a handle to it.
        let join = match join {
            Ok(join) => join,
            Err(e) => {
                shutdown.request();
                end_follower(&child_for_cleanup);
                join_reader(&reader);
                return Err(format!("failed to spawn the media thread: {e}"));
            }
        };
        Ok(ProviderHandle::new(tx, shutdown, join))
    }

    fn control(&self, command: MediaCommand, runner: &dyn CommandRunner) -> Result<(), String> {
        match command {
            MediaCommand::PlayPause => play_pause(&self.commands, &self.mpris, runner),
            MediaCommand::Previous => previous(&self.commands, &self.mpris, runner),
            MediaCommand::Next => next(&self.commands, &self.mpris, runner),
            MediaCommand::SeekTo(seconds) => seek_to(&self.commands, &self.mpris, runner, seconds),
            MediaCommand::Line(_) | MediaCommand::Disconnected(_) => Ok(()),
        }
    }
}

/// One connection's worth of following, on the reader thread.
///
/// Reports lines and the disconnect, and nothing else: every sample is made by the
/// coordinator, so the order the store sees is the order one thread produced.
/// The bool says whether the player ever spoke, which is what the backoff resets on.
fn follow_once(
    commands: &CommandsConfig,
    mpris: &MprisConfig,
    slot: &Mutex<Option<Child>>,
    lines: &std::sync::mpsc::Sender<MediaCommand>,
    stop: &Shutdown,
) -> bool {
    let mut child = match spawn_follow(commands, mpris) {
        Ok(child) => child,
        Err(e) => {
            let _ = lines.send(MediaCommand::Disconnected(format!("spawn playerctl: {e}")));
            return false;
        }
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = lines.send(MediaCommand::Disconnected(
            "playerctl stdout unavailable".into(),
        ));
        return false;
    };
    // Handed over so a stop can cut the pipe from under this thread.
    *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);

    // **After handing it over, not before.** A stop that lands while the player is
    // starting finds the slot still empty and has nothing to kill; without this check
    // the read below would begin anyway, and the wait for this thread would never end.
    // Checking before the hand-over leaves the same gap one step earlier.
    if stop.is_requested() {
        end_follower(slot);
        return false;
    }

    let mut spoke = false;
    for line in BufReader::new(stdout).lines() {
        match line {
            Ok(line) => {
                spoke = true;
                if lines.send(MediaCommand::Line(parse_line(&line))).is_err() {
                    break;
                }
            }
            Err(e) => {
                let _ = lines.send(MediaCommand::Disconnected(format!("playerctl: {e}")));
                break;
            }
        }
    }
    end_follower(slot);
    let _ = lines.send(MediaCommand::Disconnected("playerctl exited".into()));
    spoke
}

/// Take the reader thread out of the shared slot and wait for it.
fn join_reader(slot: &Mutex<Option<std::thread::JoinHandle<()>>>) {
    let reader = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(reader) = reader {
        let _ = reader.join();
    }
}

/// Take the resident `playerctl` out of the shared slot and end it.
///
/// **Taking it is the point.** Both threads reach for it - the reader when the stream
/// ends, the coordinator when the provider stops - and whoever gets there first owns
/// it. Killing through a shared reference instead would let the second one signal a
/// process id that has already been waited on, and by then it may belong to somebody
/// else entirely.
fn end_follower(slot: &Mutex<Option<Child>>) {
    let child = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(mut child) = child {
        proc::kill_group(&mut child);
    }
}

/// The longest gap between reconnection attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

#[cfg(test)]
mod tests {
    use super::*;

    /// A stop that lands while the player is starting must still be noticed.
    ///
    /// The window is between the player being handed over and the read beginning: a
    /// stop before the hand-over has nothing to kill, so if the read starts anyway
    /// nothing ever ends it. Driving the reader's own function with the stop already
    /// asked for is what pins that down - a hundred spawn-and-drop rounds only cross
    /// this window by luck.
    #[test]
    fn a_stop_during_the_players_startup_is_noticed_before_reading() {
        use std::sync::mpsc::channel;
        use std::time::Instant;

        let script = follower_script("startup-race");
        let stop = Shutdown::new();
        stop.request();

        let (lines, _rx) = channel();
        let slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
        let commands = CommandsConfig {
            playerctl: script.to_string_lossy().into_owned(),
            ..CommandsConfig::default()
        };

        // Detached rather than scoped: a reader that misses the stop settles in to read
        // and never returns, and this has to come back as a failure rather than a hang.
        let (done_tx, done_rx) = channel();
        {
            let slot = Arc::clone(&slot);
            let commands = commands.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let spoke = follow_once(&commands, &MprisConfig::default(), &slot, &lines, &stop);
                let _ = done_tx.send(spoke);
            });
        }
        let returned = done_rx.recv_timeout(Duration::from_secs(5));
        // Whatever happened, this test's player does not outlive it.
        end_follower(&slot);
        let spoke = returned.expect("the reader must return instead of settling in to read");
        assert!(!spoke, "the player never said anything");
        assert!(
            slot.lock().unwrap().is_none(),
            "and it must not leave the player behind"
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while live_players(&script) > 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(live_players(&script), 0, "playerctl left running");
    }

    /// Starting and stopping the provider must leave neither a thread nor a player.
    ///
    /// Both threads reach for the same resident process on the way out, and the reader
    /// is waited for by whichever of them gets there first. Getting that wrong leaves a
    /// `playerctl` running, or hangs on a wait that never returns.
    #[test]
    fn repeated_spawn_and_drop_leaves_no_thread_or_player() {
        let script = follower_script("spawn-drop");

        // Eight rounds, not a hundred: each stop waits out the coordinator's stop-check
        // interval, and a leak shows up in the first few either way.
        for _ in 0..8 {
            let (samples, _rx) = calloop::channel::channel();
            let provider = MediaProvider {
                commands: CommandsConfig {
                    playerctl: script.to_string_lossy().into_owned(),
                    ..CommandsConfig::default()
                },
                mpris: MprisConfig::default(),
                liveness_tick: Duration::from_secs(3600),
            }
            .spawn(samples)
            .unwrap();
            // Long enough for the reader to have the player up and be reading it.
            std::thread::sleep(Duration::from_millis(20));
            drop(provider);
        }

        // `join` returns as a thread finishes; the kernel drops its task entry a moment
        // later, so both counts are polled rather than read once.
        let deadline = Instant::now() + Duration::from_secs(5);
        while (live_threads() > 0 || live_players(&script) > 0) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(live_threads(), 0, "media threads left running");
        assert_eq!(live_players(&script), 0, "playerctl left running");
    }

    /// A player that ignores its arguments, says nothing, and stays up: the case where
    /// nothing but a kill can end the reader.
    ///
    /// It gives up after half a minute of its own accord. The real thing would not, but
    /// a test run cut short leaves this behind - it is in its own process group, which
    /// is exactly what stops the runner from taking it down.
    /// Each test gets its own copy: the two of them run side by side, and each counts
    /// the players still running by the path it started them from.
    fn follower_script(name: &str) -> std::path::PathBuf {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let script = std::env::temp_dir().join(format!("pippipit-test-follower-{name}"));
        let mut file = std::fs::File::create(&script).unwrap();
        file.write_all(b"#!/bin/sh\nsleep 30\n").unwrap();
        file.set_permissions(std::fs::Permissions::from_mode(0o755))
            .unwrap();
        script
    }

    /// The provider's own threads, by the names they are spawned under. `comm` is
    /// capped at 15 characters, which is what these are matched against.
    fn live_threads() -> usize {
        std::fs::read_dir("/proc/self/task")
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| {
                        std::fs::read_to_string(entry.path().join("comm"))
                            .map(|name| name.trim().starts_with("pippipit-media"))
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    fn live_players(script: &std::path::Path) -> usize {
        let wanted = script.to_string_lossy().into_owned();
        std::fs::read_dir("/proc")
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| {
                        std::fs::read_to_string(entry.path().join("cmdline"))
                            .map(|line| line.contains(&wanted))
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    /// A press must be answered while the reader is between connections.
    ///
    /// With no player present the reader keeps failing to connect and backs off, up to
    /// half a minute. While the reconnecting sat on the coordinator, a press landed
    /// behind that wait; now it goes straight through.
    #[test]
    fn a_press_is_run_while_the_reader_is_backing_off() {
        use std::time::Instant;

        let mut collected: Vec<MediaSample> = Vec::new();
        let mut event_loop = calloop::EventLoop::<Vec<MediaSample>>::try_new().unwrap();
        let (samples, rx) = calloop::channel::channel();
        event_loop
            .handle()
            .insert_source(rx, |message, _, out: &mut Vec<MediaSample>| {
                if let calloop::channel::Event::Msg(sample) = message {
                    out.push(sample);
                }
            })
            .unwrap();

        // A player that does not exist, so the reader never connects and is always
        // either failing or waiting to try again.
        let provider = MediaProvider {
            commands: CommandsConfig {
                playerctl: "pippipit-no-such-player".into(),
                ..CommandsConfig::default()
            },
            mpris: MprisConfig::default(),
            liveness_tick: Duration::from_secs(3600),
        }
        .spawn(samples)
        .unwrap();

        // Let the backoff grow past the moment a press would be answered by luck.
        let settle = Instant::now() + Duration::from_secs(3);
        while Instant::now() < settle {
            event_loop
                .dispatch(Some(Duration::from_millis(100)), &mut collected)
                .unwrap();
        }

        collected.clear();
        let t0 = Instant::now();
        provider.sender().send(MediaCommand::PlayPause).unwrap();

        let failed = |seen: &[MediaSample]| {
            seen.iter()
                .any(|sample| matches!(sample, MediaSample::Failed(_)))
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while !failed(&collected) && Instant::now() < deadline {
            event_loop
                .dispatch(Some(Duration::from_millis(50)), &mut collected)
                .unwrap();
        }
        assert!(
            failed(&collected),
            "the press must reach playerctl during the backoff: {:?}",
            t0.elapsed()
        );
    }

    /// A line that arrives after a button press belongs after it: folding it into the
    /// line before would report the track as it was when the button was pressed.
    #[test]
    fn a_press_keeps_its_place_between_two_lines() {
        let burst = vec![
            MediaCommand::Line(None),
            MediaCommand::PlayPause,
            MediaCommand::Line(None),
        ];
        assert_eq!(
            crate::sources::provider::coalesce(burst),
            vec![
                MediaCommand::Line(None),
                MediaCommand::PlayPause,
                MediaCommand::Line(None)
            ]
        );
    }

    /// A burst of metadata with nothing else in it is still one line.
    #[test]
    fn only_the_newest_line_of_a_burst_survives() {
        let burst = vec![MediaCommand::Line(None); 5];
        assert_eq!(
            crate::sources::provider::coalesce(burst),
            vec![MediaCommand::Line(None)]
        );
    }

    /// A line as `playerctl -F` actually emits it.
    const REAL: &str = concat!(
        "firefox\tPaused\t81000000\t265000000\tfile:///tmp/firefox-mpris/cover.png\t",
        "Sample Track\tSample Artist\tSample Album"
    );

    #[test]
    fn parses_a_real_line() {
        let m = parse_line(REAL).expect("should parse");
        assert_eq!(m.player, "firefox");
        assert_eq!(m.status, Status::Paused);
        assert_eq!(m.position_us, 81_000_000);
        assert_eq!(m.length_us, 265_000_000);
        assert_eq!(m.art_url, "file:///tmp/firefox-mpris/cover.png");
        assert_eq!(m.title, "Sample Track");
        assert_eq!(m.artist, "Sample Artist");
        assert_eq!(m.album, "Sample Album");
    }

    #[test]
    fn parses_status_values() {
        assert_eq!(Status::parse("Playing"), Status::Playing);
        assert_eq!(Status::parse("Paused"), Status::Paused);
        assert_eq!(Status::parse("Stopped"), Status::Stopped);
        // An unknown value counts as Paused, rather than falsely showing playback.
        assert_eq!(Status::parse("weird"), Status::Paused);
    }

    /// A `position 0` line can slip in right after a seek (measured). Passing it through is fine.
    #[test]
    fn accepts_intermediate_zero_position() {
        let m = parse_line("firefox\tPaused\t0\t265000000\t\tT\tA\tB").unwrap();
        assert_eq!(m.position_us, 0);
    }

    #[test]
    fn empty_and_blank_lines_are_ignored() {
        assert!(parse_line("").is_none());
        assert!(parse_line("   ").is_none());
        assert!(parse_line("\n").is_none());
    }

    #[test]
    fn missing_trailing_fields_default_to_empty() {
        let m = parse_line("firefox\tPlaying\t0\t0").unwrap();
        assert_eq!(m.art_url, "");
        assert_eq!(m.title, "");
        assert_eq!(m.artist, "");
        assert_eq!(m.album, "");
    }

    #[test]
    fn non_numeric_position_becomes_zero_not_a_panic() {
        let m = parse_line("firefox\tPlaying\tn/a\tn/a\t\tT\tA\tB").unwrap();
        assert_eq!(m.position_us, 0);
        assert_eq!(m.length_us, 0);
    }

    /// A tab in the title must not break the fixed fields.
    #[test]
    fn separator_in_title_does_not_shift_the_fixed_fields() {
        let m = parse_line("firefox\tPlaying\t5\t10\thttps://example.com/a.jpg\tti\ttle\tA\tB")
            .unwrap();
        assert_eq!(m.position_us, 5);
        assert_eq!(m.length_us, 10);
        assert_eq!(m.art_url, "https://example.com/a.jpg");
        assert_eq!(m.player, "firefox");
    }

    #[test]
    fn line_without_player_is_rejected() {
        assert!(parse_line("\tPlaying\t0\t0\tT\tA\tB").is_none());
    }
}
