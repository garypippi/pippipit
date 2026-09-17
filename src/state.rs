//! The one struct holding every widget's state, and the single point that updates it.

use chrono::{DateTime, Local};
use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};

use crate::config::Config;
use crate::event::Event;
use crate::sources::art::{ArtCommand, Placement};
use crate::sources::audio::AudioCommand;
use crate::sources::hyprland::{HyprlandCommand, HyprlandSample};
use crate::sources::media::MediaCommand;
use crate::sources::network::NetworkCommand;
use crate::sources::power::{self, PowerAction};
use crate::sources::sensors;
use crate::store::{AudioStore, MediaStore, NetworkStore, SensorStore, UiState, WorkspaceStore};
use crate::ui::hit::{Action, AudioTarget, ratio_at, volume_at};

pub struct AppState {
    pub config: Config,
    pub now: DateTime<Local>,
    pub sensors: SensorStore,
    pub audio: AudioStore,
    pub workspaces: WorkspaceStore,
    pub network: NetworkStore,
    pub media: MediaStore,
    pub ui: UiState,
    /// The channel the delayed-execution thread reports its result on.
    pub power_tx: Option<calloop::channel::Sender<(PowerAction, Result<(), String>)>>,
    /// Asks the network provider for a read. The sample comes back as `Event::Network`.
    pub network_tx: Option<std::sync::mpsc::Sender<NetworkCommand>>,
    /// Volume reads and writes both go here. Samples come back as `Event::Audio`.
    pub audio_tx: Option<std::sync::mpsc::Sender<AudioCommand>>,
    /// Play, previous, next and seek. Samples come back as `Event::Media`.
    pub media_tx: Option<std::sync::mpsc::Sender<MediaCommand>>,
    /// Workspace reads and switches. Samples come back as `Event::Workspaces`.
    pub hyprland_tx: Option<std::sync::mpsc::Sender<HyprlandCommand>>,
    /// Where the album art goes, and which window has focus. Nothing comes back.
    pub art_tx: Option<std::sync::mpsc::Sender<ArtCommand>>,
    /// The placement the art provider was last told, so an unchanged one is not sent again.
    pub art_placed: Option<Placement>,
    /// Once false, the event loop exits.
    pub running: bool,
    /// Whether a redraw is needed; the caller resets it to false after drawing.
    pub dirty: bool,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            now: Local::now(),
            sensors: SensorStore::default(),
            audio: AudioStore::default(),
            workspaces: WorkspaceStore::default(),
            network: NetworkStore::default(),
            media: MediaStore::default(),
            ui: UiState::default(),
            power_tx: None,
            network_tx: None,
            audio_tx: None,
            media_tx: None,
            hyprland_tx: None,
            art_tx: None,
            art_placed: None,
            running: true,
            dirty: true,
        }
    }

    /// The single point where state is updated.
    pub fn reduce(&mut self, event: Event) {
        match event {
            Event::Tick => {
                let now = Local::now();
                // Redraw only when the second changes.
                // The interpolated playback position rides along on that same tick.
                if now.timestamp() != self.now.timestamp() {
                    self.dirty = true;
                }
                self.now = now;
            }
            Event::Sensors(result) => {
                let history_len = self.config.general.history_len;
                self.dirty |= self.sensors.apply(result, history_len);
            }
            Event::Audio(sample) => self.dirty |= self.audio.apply(sample),
            Event::Workspaces(HyprlandSample::Focus(address)) => {
                self.send_art(ArtCommand::Focus(address))
            }
            Event::Workspaces(sample) => self.dirty |= self.workspaces.apply(sample),
            Event::Network(result) => {
                let history_len = self.config.general.history_len;
                self.dirty |= self.network.apply(result, history_len);
            }
            Event::Media(sample) => self.dirty |= self.media.apply(sample),
            Event::Refresh => {
                self.now = Local::now();
                self.refresh_sensors();
                self.refresh_audio();
                self.refresh_workspaces();
                self.refresh_network();
                self.dirty = true;
            }
            Event::Quit => self.running = false,
            Event::Input(term_event) => self.on_input(term_event),
        }
    }

    /// Re-read the temperatures synchronously. It reads `/sys` directly, so the block is momentary.
    ///
    /// State updates always go through `reduce`, to keep one update point.
    pub fn refresh_sensors(&mut self) {
        let result = sensors::read_all(&self.config.sensors).map_err(|e| e.to_string());
        self.reduce(Event::Sensors(result));
    }

    /// Ask for a fresh volume read. The provider answers with `Event::Audio` later.
    pub fn refresh_audio(&mut self) {
        self.send_audio(AudioCommand::Refresh);
    }

    /// Hand a volume command to the provider. A burst of them collapses on the way.
    fn send_audio(&mut self, command: AudioCommand) {
        if let Some(tx) = &self.audio_tx {
            let _ = tx.send(command);
        }
    }

    fn on_input(&mut self, event: TermEvent) {
        match event {
            TermEvent::Key(key) => self.on_key(key),
            TermEvent::Mouse(mouse) => self.on_mouse(mouse),
            TermEvent::Resize(_, _) => self.dirty = true,
            _ => {}
        }
    }

    /// Ask for a fresh workspace read. The provider answers with `Event::Workspaces` later.
    pub fn refresh_workspaces(&mut self) {
        self.send_hyprland(HyprlandCommand::Refresh);
    }

    /// Hand a command to the Hyprland provider. Socket events collapse there, not here.
    fn send_hyprland(&mut self, command: HyprlandCommand) {
        if let Some(tx) = &self.hyprland_tx {
            let _ = tx.send(command);
        }
    }

    /// Ask for a fresh network read. The provider answers with `Event::Network` later.
    pub fn refresh_network(&mut self) {
        if let Some(tx) = &self.network_tx {
            let _ = tx.send(NetworkCommand::Refresh);
        }
    }

    fn on_mouse(&mut self, mouse: MouseEvent) {
        let action = self.ui.hits.at(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => match action {
                Some(Action::VolumeBar { target, x, width }) => {
                    let percent = volume_at(x, width, mouse.column);
                    self.set_volume(target, percent);
                }
                Some(Action::ToggleMute { target }) => self.toggle_mute(target),
                Some(Action::Workspace { id }) => self.switch_workspace(id),
                Some(Action::SeekBar { x, width }) => {
                    let ratio = ratio_at(x, width, mouse.column);
                    self.seek(ratio);
                }
                Some(Action::Previous) => self.send_media(MediaCommand::Previous),
                Some(Action::PlayPause) => self.send_media(MediaCommand::PlayPause),
                Some(Action::Next) => self.send_media(MediaCommand::Next),
                Some(Action::Power(action)) => self.request_power(action),
                Some(Action::ConfirmYes) => self.confirm_power(true),
                Some(Action::ConfirmNo) | Some(Action::ConfirmCancel) => self.confirm_power(false),
                Some(Action::NetworkSelect { index }) => self.select_network(index as usize),
                None => {}
            },
            // The wheel is volume, and only over the volume rows; picking an interface is
            // a click on its name. It is never bound to track skipping - that invites
            // accidents.
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                // It works over the icon as well as the bar, so a wheel that misses by
                // a column or two still does what was meant - on **that row's** device.
                let target = match action {
                    Some(Action::VolumeBar { target, .. })
                    | Some(Action::ToggleMute { target }) => target,
                    _ => return,
                };
                let step = self.config.audio.step as i16;
                self.nudge_volume(
                    target,
                    if mouse.kind == MouseEventKind::ScrollUp {
                        step
                    } else {
                        -step
                    },
                );
            }
            _ => {}
        }
    }

    /// Show a different interface.
    ///
    /// The index names a position in the list as it stood when the row was drawn, and
    /// **what is kept is the name it resolves to**. The list is re-sorted on every
    /// refresh, so an index held across one would drift: a docker container starting is
    /// enough to slide it onto a different interface.
    fn select_network(&mut self, index: usize) {
        let Some(iface) = self.network.latest.interfaces.get(index) else {
            return;
        };
        if self.network.selected.as_deref() != Some(iface.name.as_str()) {
            self.network.selected = Some(iface.name.clone());
            self.dirty = true;
        }
    }

    /// Switch to a workspace.
    ///
    /// Nothing is refreshed here on purpose: Hyprland answers the switch with a
    /// `workspace` event on socket2, and the subscription already redraws from that.
    fn switch_workspace(&mut self, id: i64) {
        self.workspaces.switch_error = None;
        self.send_hyprland(HyprlandCommand::Switch(id));
    }

    fn set_volume(&mut self, target: AudioTarget, percent: u16) {
        self.send_audio(AudioCommand::Set(target, percent));
    }

    fn nudge_volume(&mut self, target: AudioTarget, delta: i16) {
        self.send_audio(AudioCommand::Nudge(target, delta));
    }

    /// A power button was pressed. **The confirmation modal is always shown**, except that screen off may skip it by config.
    fn request_power(&mut self, action: PowerAction) {
        // Refuse while an action is pending. Pressing again during the one-second wait
        // would queue a second suspend or poweroff.
        if self.ui.power_running.is_some() {
            return;
        }
        if action.may_skip_confirm(&self.config.power) {
            self.run_power(action);
            return;
        }
        self.ui.pending_power = Some(action);
        // Focus always defaults to No.
        self.ui.confirm_yes_focused = false;
        self.dirty = true;
    }

    fn confirm_power(&mut self, yes: bool) {
        let Some(action) = self.ui.pending_power.take() else {
            return;
        };
        self.dirty = true;
        if yes {
            self.run_power(action);
        }
    }

    /// Run the power command **on its own thread**.
    ///
    /// `power::run` blocks for `delay_ms`, so waiting on it here would freeze the UI.
    fn run_power(&mut self, action: PowerAction) {
        if self.ui.power_running.is_some() {
            return;
        }
        self.ui.power_running = Some(action);
        self.ui.power_error = None;
        self.dirty = true;

        let config = self.config.power.clone();
        let commands = self.config.commands.clone();
        let Some(tx) = self.power_tx.clone() else {
            // No channel means a unit test; resolve it in place without the delay.
            if let Err(message) = power::run_now(action, &config, &commands) {
                self.ui.power_error = Some(message);
            }
            self.ui.power_running = None;
            return;
        };
        // If the thread cannot start, put `power_running` back.
        // Swallowing the error would leave it stuck showing "running" and unpressable.
        if let Err(err) = std::thread::Builder::new()
            .name("pippipit-power".into())
            .spawn(move || {
                let result = power::run(action, &config, &commands);
                let _ = tx.send((action, result));
            })
        {
            self.ui.power_running = None;
            self.ui.power_error = Some(format!("failed to start power thread: {err}"));
        }
    }

    /// Receive the result of the delayed execution.
    pub fn on_power_result(&mut self, action: PowerAction, result: Result<(), String>) {
        if self.ui.power_running == Some(action) {
            self.ui.power_running = None;
        }
        if let Err(message) = result {
            self.ui.power_error = Some(message);
        }
        self.dirty = true;
    }

    /// Keys while the modal is up. Everything is consumed here and nothing falls through.
    fn on_confirm_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => self.confirm_power(true),
            KeyCode::Enter => {
                let yes = self.ui.confirm_yes_focused;
                self.confirm_power(yes);
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => {
                self.confirm_power(false)
            }
            KeyCode::Left
            | KeyCode::Right
            | KeyCode::Tab
            | KeyCode::Char('h')
            | KeyCode::Char('l') => {
                self.ui.confirm_yes_focused = !self.ui.confirm_yes_focused;
                self.dirty = true;
            }
            _ => {}
        }
    }

    fn seek(&mut self, ratio: f64) {
        let Some(media) = &self.media.latest else {
            return;
        };
        if media.length_us == 0 {
            return;
        }
        let seconds = ratio * (media.length_us as f64) / 1_000_000.0;
        self.send_media(MediaCommand::SeekTo(seconds));
    }

    /// Hand a transport command to the media provider. Failures come back as samples.
    fn send_media(&mut self, command: MediaCommand) {
        if let Some(tx) = &self.media_tx {
            let _ = tx.send(command);
        }
    }

    /// Tell the art provider where the art goes after a draw, if that changed.
    ///
    /// The frame comes from the hit table the draw just built, and the art only goes in
    /// it when the track has some. `terminal` is the terminal's columns and rows.
    pub fn place_art(&mut self, terminal: (u16, u16)) {
        let placement = self.ui.hits.art.and_then(|frame| {
            let media = self.media.latest.as_ref()?;
            Some(Placement {
                url: crate::sources::art::url_for(media)?,
                x: frame.x,
                y: frame.y,
                width: frame.width,
                height: frame.height,
                terminal,
            })
        });
        if placement == self.art_placed {
            return;
        }
        let command = match &placement {
            Some(placement) => ArtCommand::Show(placement.clone()),
            None => ArtCommand::Hide,
        };
        self.art_placed = placement;
        self.send_art(command);
    }

    fn send_art(&mut self, command: ArtCommand) {
        if let Some(tx) = &self.art_tx {
            let _ = tx.send(command);
        }
    }

    fn toggle_mute(&mut self, target: AudioTarget) {
        self.send_audio(AudioCommand::ToggleMute(target));
    }

    /// Only pippipit's own keys are handled.
    ///
    /// Volume, playback and workspace switching belong to the Hyprland keybinds, not here.
    fn on_key(&mut self, key: KeyEvent) {
        // Avoid picking up the Windows-style KeyEventKind::Release a second time.
        if key.kind != KeyEventKind::Press {
            return;
        }
        // While the modal is up, nothing falls through.
        if self.ui.pending_power.is_some() {
            self.on_confirm_key(key);
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => self.reduce(Event::Quit),
            KeyCode::Char('q') => self.reduce(Event::Quit),
            KeyCode::Char('r') => self.reduce(Event::Refresh),
            KeyCode::Char('p') => {
                self.network.redact = !self.network.redact;
                self.dirty = true;
            }
            KeyCode::Char('?') => {
                self.ui.show_help = !self.ui.show_help;
                self.dirty = true;
            }
            KeyCode::Esc if self.ui.show_help => {
                self.ui.show_help = false;
                self.dirty = true;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn state() -> AppState {
        AppState::new(Config::default())
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    /// **Suspend and Power Off always go through the confirmation.**
    #[test]
    fn destructive_actions_open_the_modal_instead_of_running() {
        for action in [PowerAction::Suspend, PowerAction::PowerOff] {
            let mut s = state();
            s.request_power(action);
            assert_eq!(
                s.ui.pending_power,
                Some(action),
                "{action:?} must ask first"
            );
            // Focus defaults to No.
            assert!(!s.ui.confirm_yes_focused, "default focus must be No");
        }
    }

    /// Screen off skips the confirmation by default, since it does no harm.
    #[test]
    fn screen_off_skips_the_modal_by_default() {
        let mut s = state();
        s.request_power(PowerAction::ScreenOff);
        assert_eq!(s.ui.pending_power, None);
    }

    #[test]
    fn screen_off_can_be_made_to_confirm() {
        let mut s = state();
        s.config.power.confirm_screen_off = true;
        s.request_power(PowerAction::ScreenOff);
        assert_eq!(s.ui.pending_power, Some(PowerAction::ScreenOff));
    }

    /// **Enter while the modal is up does nothing, because focus defaults to No.**
    #[test]
    fn enter_on_default_focus_cancels() {
        let mut s = state();
        s.request_power(PowerAction::PowerOff);
        s.on_key(key(KeyCode::Enter));
        assert_eq!(s.ui.pending_power, None, "modal must close");
        assert!(
            s.ui.power_error.is_none(),
            "nothing should have been executed"
        );
    }

    /// The screenshot key. Nothing else about the display changes, and pressing it
    /// again puts it back.
    #[test]
    fn p_toggles_the_redaction() {
        let mut s = state();
        assert!(!s.network.redact);
        s.on_key(key(KeyCode::Char('p')));
        assert!(s.network.redact);
        assert!(s.dirty, "the screen has to be redrawn");
        s.on_key(key(KeyCode::Char('p')));
        assert!(!s.network.redact);
    }

    #[test]
    fn esc_and_n_cancel() {
        for code in [KeyCode::Esc, KeyCode::Char('n')] {
            let mut s = state();
            s.request_power(PowerAction::Suspend);
            s.on_key(key(code));
            assert_eq!(s.ui.pending_power, None, "{code:?} must cancel");
        }
    }

    /// While the modal is up, `q` cancels instead of quitting.
    #[test]
    fn q_cancels_instead_of_quitting_while_the_modal_is_open() {
        let mut s = state();
        s.request_power(PowerAction::PowerOff);
        s.on_key(key(KeyCode::Char('q')));
        assert_eq!(s.ui.pending_power, None);
        assert!(s.running, "must not quit while confirming");
    }

    /// While the modal is up, the underlying keys (r / ? / q) never see the input.
    #[test]
    fn modal_swallows_other_keys() {
        let mut s = state();
        s.request_power(PowerAction::PowerOff);
        s.on_key(key(KeyCode::Char('?')));
        assert!(!s.ui.show_help, "help must not open behind the modal");
        assert_eq!(
            s.ui.pending_power,
            Some(PowerAction::PowerOff),
            "modal stays"
        );
    }

    #[test]
    fn arrows_move_focus_between_yes_and_no() {
        let mut s = state();
        s.request_power(PowerAction::PowerOff);
        assert!(!s.ui.confirm_yes_focused);
        s.on_key(key(KeyCode::Left));
        assert!(s.ui.confirm_yes_focused);
        s.on_key(key(KeyCode::Right));
        assert!(!s.ui.confirm_yes_focused);
    }

    /// **Pressing again during the delay must not queue a second action.**
    /// If a re-click landed during the one-second wait, suspend would be issued twice.
    #[test]
    fn power_requests_are_ignored_while_one_is_running() {
        let mut s = state();
        s.ui.power_running = Some(PowerAction::Suspend);
        s.request_power(PowerAction::PowerOff);
        assert_eq!(s.ui.pending_power, None, "must not open a second modal");
        assert_eq!(s.ui.power_running, Some(PowerAction::Suspend), "unchanged");
    }

    #[test]
    fn confirming_twice_only_runs_once() {
        let mut s = state();
        // Use a command that fails, so the test does not actually suspend.
        s.config.power.suspend = vec!["pippipit-no-such-command".into()];
        s.config.power.delay_ms = 0;
        s.request_power(PowerAction::Suspend);
        s.confirm_power(true);
        // The first attempt errors out and power_running is already cleared.
        assert!(s.ui.power_error.is_some());
        // The modal is closed, so the second confirm does nothing.
        s.confirm_power(true);
        assert_eq!(s.ui.pending_power, None);
    }

    /// Normally there are only three keys; volume and workspace keys do not exist here.
    #[test]
    fn only_three_keys_are_owned() {
        let mut s = state();
        for code in [
            KeyCode::Char('1'),
            KeyCode::Char('h'),
            KeyCode::Char('l'),
            KeyCode::Char('-'),
            KeyCode::Char('='),
            KeyCode::Char(' '),
            KeyCode::Char('s'),
            KeyCode::Char('z'),
            KeyCode::Char('p'),
        ] {
            s.on_key(key(code));
            assert!(s.running, "{code:?} must not quit");
            assert_eq!(s.ui.pending_power, None, "{code:?} must not touch power");
            assert!(!s.ui.show_help, "{code:?} must not open help");
        }
        s.on_key(key(KeyCode::Char('?')));
        assert!(s.ui.show_help);
        s.on_key(key(KeyCode::Char('q')));
        assert!(!s.running);
    }
}
