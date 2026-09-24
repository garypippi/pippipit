//! pippipit - a desktop status TUI built around Hyprland.
//!
//! Providers wait on the outside world from their own threads; calloop multiplexes what
//! they send back, and only this thread touches the state or the terminal.

mod config;
mod event;
mod sources;
mod state;
mod store;
mod ui;
mod util;

use std::io::{self, Stdout};
use std::time::Duration;

use anyhow::{Context, Result};
use calloop::EventLoop;
use calloop::channel::{Event as ChannelEvent, channel};
use calloop::signals::{Signal, Signals};
use calloop::timer::{TimeoutAction, Timer};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as TermEvent, read as read_term,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::config::{Config, SignalName};
use crate::event::Event;
use crate::state::AppState;

type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Command-line arguments. There are few enough to parse by hand; clap would not earn its place.
struct Cli {
    config: Option<std::path::PathBuf>,
    no_mouse: bool,
}

const USAGE: &str = "\
pippipit — Hyprland status TUI

USAGE:
    pippipit [OPTIONS]

OPTIONS:
    -c, --config <PATH>   Use this config file instead of
                          $XDG_CONFIG_HOME/pippipit/config.toml
        --no-mouse        Do not capture the mouse
                          (keeps the terminal's own text selection working)
    -h, --help            Show this help
    -V, --version         Show the version

KEYS:
    r   refresh all sources
    p   hide the SSID and the address, for a screenshot
    ?   help overlay
    q   quit

Volume, playback and workspace switching are Hyprland keybinds, not pippipit's.
";

fn parse_args() -> Result<Option<Cli>> {
    let mut cli = Cli {
        config: None,
        no_mouse: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("pippipit {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--no-mouse" => cli.no_mouse = true,
            "-c" | "--config" => {
                let path = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--config needs a path"))?;
                cli.config = Some(path.into());
            }
            other => anyhow::bail!("unknown argument: {other}\n\n{USAGE}"),
        }
    }
    Ok(Some(cli))
}

fn main() -> Result<()> {
    let Some(cli) = parse_args()? else {
        return Ok(());
    };
    let mut config = match &cli.config {
        Some(path) => Config::load_from(path)?,
        None => Config::load()?,
    };
    if cli.no_mouse {
        config.input.mouse = false;
    }

    // Block the signals the panel listens for, before anything else spawns a thread.
    //
    // The signalfd behind the event loop only receives a signal that every thread blocks;
    // one thread left unblocked is enough for the kernel to deliver there instead, and the
    // default action for `SIGUSR1` is to terminate the process. A mask is per-thread and is
    // inherited at spawn, so blocking here - while this is the only thread - covers every
    // thread the run goes on to create, whatever order they are created in.
    block_signals(&[
        to_raw_signal(config.signals.audio),
        to_raw_signal(config.signals.refresh_all),
    ])?;

    // Install the hook before entering raw mode, so a panic cannot leave the terminal wrecked.
    install_panic_hook();

    let mouse = config.input.mouse;

    // From here on, **every failure path restores the terminal**.
    //
    // A `?` fired after raw mode is enabled would exit without restoring it.
    // A mid-way failure in `setup_terminal` and `terminal.clear()?` are both such exits.
    // Wrapping the whole initialisation means the restore runs regardless of the result.
    enable_raw_mode().context("failed to enter raw mode")?;
    let result = (|| -> Result<()> {
        let mut terminal = enter_screen(mouse).context("failed to set up terminal")?;
        terminal.clear()?;
        // The hook aborts, so the panic path can only be exercised from another
        // process - and only from here, with raw mode on and the alternate screen up,
        // is there anything for it to put back.
        if std::env::var_os("PIPPIPIT_PANIC_ON_THREAD").is_some() {
            let _ = std::thread::spawn(|| panic!("provider thread panicked")).join();
        }
        run(&mut terminal, config)
    })();
    let restored = restore_terminal();
    result.and(restored)
}

/// The provider threads the panel runs, stopped as a group.
///
/// Dropped one at a time they each take up to their stop-check interval, and the waits
/// add up; asked to stop together they wind down side by side. The fields are dropped
/// after this runs, which is where each one is waited for.
struct Providers {
    network: sources::provider::ProviderHandle<sources::network::NetworkCommand>,
    audio: sources::provider::ProviderHandle<sources::audio::AudioCommand>,
    media: sources::provider::ProviderHandle<sources::media::MediaCommand>,
    hyprland: sources::provider::ProviderHandle<sources::hyprland::HyprlandCommand>,
    art: Option<sources::provider::ProviderHandle<sources::art::ArtCommand>>,
}

impl Drop for Providers {
    fn drop(&mut self) {
        self.network.stop();
        self.audio.stop();
        self.media.stop();
        self.hyprland.stop();
        if let Some(art) = &self.art {
            art.stop();
        }
    }
}

fn run(terminal: &mut Tui, config: Config) -> Result<()> {
    // App wants the Terminal, but the caller owns it, so a wrapped reference is passed instead.
    let mut event_loop: EventLoop<AppCtx<'_>> =
        EventLoop::try_new().context("failed to create event loop")?;
    let handle = event_loop.handle();

    let tick = Duration::from_millis(config.general.tick_ms.max(50));

    handle
        .insert_source(Timer::from_duration(tick), move |_deadline, _, ctx| {
            ctx.state.reduce(Event::Tick);
            TimeoutAction::ToDuration(tick)
        })
        .map_err(|err| anyhow::anyhow!("failed to insert timer source: {err}"))?;

    let sensor_tick = Duration::from_millis(config.general.sensor_tick_ms.max(200));
    handle
        .insert_source(
            Timer::from_duration(sensor_tick),
            move |_deadline, _, ctx: &mut AppCtx<'_>| {
                ctx.state.refresh_sensors();
                TimeoutAction::ToDuration(sensor_tick)
            },
        )
        .map_err(|err| anyhow::anyhow!("failed to insert sensor timer source: {err}"))?;

    let network_tick = Duration::from_millis(config.general.network_tick_ms.max(500));
    // The network reads on its own thread and pushes each sample here. Its own tick paces
    // it, and the handle stops the thread when it goes out of scope at the end of `run`.
    let (network_samples, network_rx) = channel::<Result<sources::network::Network, String>>();
    let network = sources::network::NetworkProvider {
        config: config.network.clone(),
        commands: config.commands.clone(),
        tick: network_tick,
    }
    .spawn(network_samples)
    .map_err(|err| anyhow::anyhow!("{err}"))?;
    handle
        .insert_source(network_rx, |message, _, ctx: &mut AppCtx<'_>| {
            if let ChannelEvent::Msg(sample) = message {
                ctx.state.reduce(Event::Network(sample));
            }
        })
        .map_err(|err| anyhow::anyhow!("failed to insert network source: {err}"))?;

    // Volume reads and writes share one thread, so a wheel spin cannot pile up: the
    // notches waiting behind the first one collapse into a single `pactl` call.
    let (audio_samples, audio_rx) = channel::<sources::audio::AudioSample>();
    let audio = sources::audio::AudioProvider {
        config: config.audio.clone(),
        commands: config.commands.clone(),
        tick: (config.audio.poll_ms > 0)
            .then(|| Duration::from_millis(config.audio.poll_ms.max(500))),
    }
    .spawn(audio_samples)
    .map_err(|err| anyhow::anyhow!("{err}"))?;
    handle
        .insert_source(audio_rx, |message, _, ctx: &mut AppCtx<'_>| {
            if let ChannelEvent::Msg(sample) = message {
                ctx.state.reduce(Event::Audio(sample));
            }
        })
        .map_err(|err| anyhow::anyhow!("failed to insert audio source: {err}"))?;

    // A signal triggers an immediate refresh (a Hyprland keybind sends it with `pkill`).
    let audio_signal = to_signal(config.signals.audio);
    let refresh_signal = to_signal(config.signals.refresh_all);
    handle
        .insert_source(
            Signals::new(&[audio_signal, refresh_signal]).context("failed to set up signals")?,
            move |event, _, ctx: &mut AppCtx<'_>| {
                if event.signal() == audio_signal {
                    ctx.state.refresh_audio();
                } else {
                    ctx.state.reduce(Event::Refresh);
                }
            },
        )
        .map_err(|err| anyhow::anyhow!("failed to insert signal source: {err}"))?;

    // A power command waits `delay_ms` before running.
    // To keep the UI alive during that wait, it runs on its own thread and only the result comes back.
    let (power_tx, power_rx) = channel::<(sources::power::PowerAction, Result<(), String>)>();
    handle
        .insert_source(power_rx, |message, _, ctx: &mut AppCtx<'_>| {
            if let ChannelEvent::Msg((action, result)) = message {
                ctx.state.on_power_result(action, result);
            }
        })
        .map_err(|err| anyhow::anyhow!("failed to insert power source: {err}"))?;

    // mpris arrives as pushes from a resident `playerctl -F`, read on a thread of its own.
    // The transport controls run there too, in the order they were pressed.
    let (media_samples, media_rx) = channel::<sources::media::MediaSample>();
    let media = sources::media::MediaProvider {
        commands: config.commands.clone(),
        mpris: config.mpris.clone(),
        liveness_tick: network_tick,
    }
    .spawn(media_samples)
    .map_err(|err| anyhow::anyhow!("{err}"))?;
    handle
        .insert_source(media_rx, |message, _, ctx: &mut AppCtx<'_>| {
            if let ChannelEvent::Msg(sample) = message {
                ctx.state.reduce(Event::Media(sample));
            }
        })
        .map_err(|err| anyhow::anyhow!("failed to insert media source: {err}"))?;

    // Hyprland's socket2 is watched on a reader thread, which only reports. The
    // coordinator behind it turns a burst of events into a single re-read, and answers
    // workspace switches in the order they were clicked.
    let (hypr_samples, hypr_rx) = channel::<sources::hyprland::HyprlandSample>();
    let hyprland = sources::hyprland::HyprlandProvider {
        workspaces: config.workspaces.clone(),
        commands: config.commands.clone(),
        // Only the art cares which window has focus.
        focus: config.art.enabled,
    }
    .spawn(hypr_samples)
    .map_err(|err| anyhow::anyhow!("{err}"))?;
    handle
        .insert_source(hypr_rx, |message, _, ctx: &mut AppCtx<'_>| {
            if let ChannelEvent::Msg(sample) = message {
                ctx.state.reduce(Event::Workspaces(sample));
            }
        })
        .map_err(|err| anyhow::anyhow!("failed to insert hyprland source: {err}"))?;

    // The album art has a resident Überzug++ of its own. It is told where to draw after
    // each draw and which window has focus as that changes, and says nothing back.
    let art = config
        .art
        .enabled
        .then(|| {
            sources::art::ArtProvider {
                config: config.art.clone(),
                commands: config.commands.clone(),
            }
            .spawn()
        })
        .transpose()
        .map_err(|err| anyhow::anyhow!("{err}"))?;

    // Terminal input comes from a **blocking read on a dedicated thread**.
    //
    // Watching stdin through calloop's `Generic` (level-triggered) can enter a state where
    // the fd stays readable but crossterm cannot assemble an event, at which point
    // dispatch returns immediately over and over: **a 100% spin with nothing to show for it**.
    // One run burned 132 seconds of CPU over 392 seconds of uptime.
    // The cause was never pinned down, so the mechanism that could produce it was removed outright.
    // With a blocking read, only channels and timers can wake the event loop.
    let (tx, rx) = channel::<TermEvent>();
    std::thread::Builder::new()
        .name("pippipit-input".into())
        .spawn(move || {
            // Leave when the terminal closes (Err) or the receiver is gone (send fails).
            while let Ok(event) = read_term() {
                if tx.send(event).is_err() {
                    break;
                }
            }
        })
        .context("failed to spawn input thread")?;

    handle
        .insert_source(rx, |message, _, ctx: &mut AppCtx<'_>| {
            if let ChannelEvent::Msg(event) = message {
                ctx.state.reduce(Event::Input(event));
            }
        })
        .map_err(|err| anyhow::anyhow!("failed to insert input source: {err}"))?;

    let mut ctx = AppCtx {
        state: AppState::new(config),
        terminal,
    };
    ctx.state.power_tx = Some(power_tx);
    ctx.state.network_tx = Some(network.sender());
    ctx.state.audio_tx = Some(audio.sender());
    ctx.state.media_tx = Some(media.sender());
    ctx.state.hyprland_tx = Some(hyprland.sender());
    ctx.state.art_tx = art.as_ref().map(|art| art.sender());

    // From here they are held together, so that quitting asks all of them to stop before
    // waiting on any of them.
    let _providers = Providers {
        network,
        audio,
        media,
        hyprland,
        art,
    };

    // The temperatures are read here and now, since nothing else will until the first
    // tick; the workspaces are asked for, and answer when the provider gets to it.
    // Network, audio and media need neither: each opens with a read of its own.
    ctx.state.refresh_sensors();
    ctx.state.refresh_workspaces();

    // The first draw.
    ctx.redraw()?;

    while ctx.state.running {
        event_loop
            .dispatch(Some(Duration::from_millis(200)), &mut ctx)
            .context("event loop dispatch failed")?;
        ctx.redraw()?;
    }
    Ok(())
}

/// A borrowing flavour of `App`, so that `main` keeps ownership of the `Terminal`.
struct AppCtx<'a> {
    state: AppState,
    terminal: &'a mut Tui,
}

impl AppCtx<'_> {
    fn redraw(&mut self) -> Result<()> {
        if !self.state.dirty {
            return Ok(());
        }
        // The hit table is rebuilt on every draw. state and terminal are separate fields,
        // so the borrows do not conflict.
        let mut hits = std::mem::take(&mut self.state.ui.hits);
        let state = &self.state;
        let result = self
            .terminal
            .draw(|frame| ui::draw(frame, state, &mut hits));
        self.state.ui.hits = hits;
        result?;
        // Where the art goes is only known once the pane has been laid out.
        let size = self.terminal.size()?;
        self.state.place_art((size.width, size.height));
        self.state.dirty = false;
        Ok(())
    }
}

fn to_signal(name: SignalName) -> Signal {
    match name {
        SignalName::Usr1 => Signal::SIGUSR1,
        SignalName::Usr2 => Signal::SIGUSR2,
    }
}

fn to_raw_signal(name: SignalName) -> libc::c_int {
    match name {
        SignalName::Usr1 => libc::SIGUSR1,
        SignalName::Usr2 => libc::SIGUSR2,
    }
}

/// Add `signals` to the calling thread's blocked set.
fn block_signals(signals: &[libc::c_int]) -> Result<()> {
    // SAFETY: `set` is filled by `sigemptyset` before it is read, and every call is
    // handed a pointer to a live local of the right type.
    let ok = unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for &signal in signals {
            libc::sigaddset(&mut set, signal);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut())
    };
    if ok != 0 {
        return Err(std::io::Error::from_raw_os_error(ok)).context("failed to block signals");
    }
    Ok(())
}

/// Enter the alternate screen and build the `Terminal`.
///
/// **The caller must have enabled raw mode first.**
/// A failure here still leaves the caller's restore path guaranteed to run.
fn enter_screen(mouse: bool) -> Result<Tui> {
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    if mouse {
        execute!(stdout, EnableMouseCapture)?;
    }
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal() -> Result<()> {
    let mut stdout = io::stdout();
    // One failing step must not stop the rest.
    let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen);
    disable_raw_mode()?;
    Ok(())
}

/// Restore the terminal on a panic, then end the process.
///
/// A panic on a provider thread would otherwise kill only that thread, leaving a panel
/// where the clock still ticks and one row never updates again. Aborting from the hook
/// makes any panic, on any thread, the end of the run.
///
/// The restore is best-effort: a failure here must not panic, or the process dies with
/// the terminal still in raw mode.
fn install_panic_hook() {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal();
        hook(info);
        std::process::abort();
    }));
}
