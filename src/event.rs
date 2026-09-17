//! The events that flow from every source into AppState.
//!
//! Sources only emit an Event; state updates all funnel through `AppState::reduce`.

use crossterm::event::Event as TermEvent;

use crate::sources::audio::AudioSample;
use crate::sources::hyprland::HyprlandSample;
use crate::sources::media::MediaSample;
use crate::sources::network::Network;
use crate::sources::sensors::Reading;

#[derive(Debug, Clone)]
pub enum Event {
    /// The one-second tick (clock and friends).
    Tick,
    /// Terminal input: keys, mouse, resize.
    Input(TermEvent),
    /// A temperature refresh finished.
    Sensors(Result<Vec<Reading>, String>),
    /// The audio provider reported: a read, or a write that failed.
    Audio(AudioSample),
    /// The Hyprland provider reported: a read, or a switch that was refused.
    Workspaces(HyprlandSample),
    /// A network refresh finished.
    Network(Result<Network, String>),
    /// The media provider reported: a new track state, or a failure.
    Media(MediaSample),
    /// Force a refresh of every source.
    Refresh,
    /// A request to quit.
    Quit,
}
