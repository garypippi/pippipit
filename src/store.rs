//! The state each source contributes, and what is derived from it.
//!
//! Providers hand over one raw sample at a time. Everything that outlives a sample -
//! history, rates, the interpolated playback position - is built here instead, so a
//! provider restarting does not take the history with it.
//!
//! These are pure and synchronous. Only the event loop's thread touches them.

use std::collections::HashMap;
use std::time::Instant;

use crate::sources::audio::{Audio, AudioSample};
use crate::sources::hyprland::{HyprlandSample, Workspaces};
use crate::sources::media::{Media, MediaSample, Status};
use crate::sources::network::Network;
use crate::sources::power::PowerAction;
use crate::sources::sensors::Reading;
use crate::ui::hit::HitMap;
use crate::util::sparkline::Ring;

/// Temperatures, and the sparkline history behind them.
#[derive(Default)]
pub struct SensorStore {
    /// The latest readings; empty means nothing has been read yet.
    pub latest: Vec<Reading>,
    /// History for the sparklines, keyed by `Reading::key`.
    pub history: HashMap<String, Ring>,
    /// The message shown when a read fails.
    pub error: Option<String>,
}

impl SensorStore {
    /// Take one sample. The bool says whether the screen needs redrawing.
    pub fn apply(&mut self, result: Result<Vec<Reading>, String>, history_len: usize) -> bool {
        match result {
            Ok(readings) => {
                for reading in &readings {
                    self.history
                        .entry(reading.key.clone())
                        .or_insert_with(|| Ring::new(history_len))
                        .push(reading.celsius);
                }
                self.latest = readings;
                self.error = None;
            }
            // A failed read is not fatal: keep the last value and surface the reason.
            Err(message) => self.error = Some(message),
        }
        true
    }
}

/// The volume of the default sink and source.
#[derive(Default)]
pub struct AudioStore {
    pub latest: Audio,
    pub error: Option<String>,
}

impl AudioStore {
    pub fn apply(&mut self, sample: AudioSample) -> bool {
        match sample {
            AudioSample::State(Ok(audio)) => {
                self.latest = audio;
                self.error = None;
            }
            // A failed read is not fatal: keep the last value and surface the reason.
            AudioSample::State(Err(message)) | AudioSample::CommandFailed(message) => {
                self.error = Some(message)
            }
        }
        true
    }
}

/// The workspace map.
#[derive(Default)]
pub struct WorkspaceStore {
    pub latest: Workspaces,
    pub error: Option<String>,
    /// The message from a refused switch. Read failures live in `error`; this one is
    /// about a click, so it is shown on the status line.
    pub switch_error: Option<String>,
}

impl WorkspaceStore {
    pub fn apply(&mut self, sample: HyprlandSample) -> bool {
        match sample {
            HyprlandSample::State(Ok(workspaces)) => {
                self.error = None;
                // No change means no redraw, which saves work during an event storm.
                let changed = self.latest != workspaces;
                self.latest = workspaces;
                changed
            }
            HyprlandSample::State(Err(message)) => {
                self.error = Some(message);
                true
            }
            // Which window has focus is the art provider's business; nothing drawn shows it.
            HyprlandSample::Focus(_) => false,
            HyprlandSample::SwitchFailed(message) => {
                self.switch_error = Some(message);
                true
            }
        }
    }
}

/// Interfaces, plus the throughput derived from their counters.
#[derive(Default)]
pub struct NetworkStore {
    pub latest: Network,
    pub error: Option<String>,
    /// The latest throughput per interface, in bytes/sec.
    pub rates: HashMap<String, (f64, f64)>,
    pub rx_history: HashMap<String, Ring>,
    pub tx_history: HashMap<String, Ring>,
    /// The interface the network pane is showing, by name. `None` means "the first
    /// one", which `sources::network::order` has already made the most useful.
    ///
    /// Nothing resets this when the list changes - an interface going away while
    /// somebody is reading it is no reason to yank the pane back to the top - it is
    /// clamped against what the last draw could actually show.
    pub selected: Option<String>,
    /// Cover up what identifies this machine's network, for a screenshot.
    ///
    /// A key rather than a flag: the moment you want it is the moment you are about to
    /// take the picture, and quitting and restarting to get there would mean the
    /// picture no longer shows what you wanted to show.
    pub redact: bool,
    /// The previous sample the rate is derived from: cumulative counters and a timestamp.
    prev: HashMap<String, (u64, u64, Instant)>,
}

impl NetworkStore {
    pub fn apply(&mut self, result: Result<Network, String>, history_len: usize) -> bool {
        match result {
            Ok(network) => {
                self.update_rates(&network, history_len);
                self.latest = network;
                self.error = None;
            }
            Err(message) => self.error = Some(message),
        }
        true
    }

    /// Derive bytes/sec from the delta of the cumulative counters.
    ///
    /// An interface disappearing or being recreated rewinds the counters, so
    /// **a decrease discards the delta and counts as 0**, never a negative rate.
    fn update_rates(&mut self, network: &Network, history_len: usize) {
        let now = Instant::now();
        for iface in &network.interfaces {
            if let Some(&(prev_rx, prev_tx, prev_at)) = self.prev.get(&iface.name) {
                let secs = now.duration_since(prev_at).as_secs_f64();
                if secs > 0.0 {
                    let rx = iface.rx_bytes.saturating_sub(prev_rx) as f64 / secs;
                    let tx = iface.tx_bytes.saturating_sub(prev_tx) as f64 / secs;
                    self.rates.insert(iface.name.clone(), (rx, tx));
                    self.rx_history
                        .entry(iface.name.clone())
                        .or_insert_with(|| Ring::new(history_len))
                        .push(rx as f32);
                    self.tx_history
                        .entry(iface.name.clone())
                        .or_insert_with(|| Ring::new(history_len))
                        .push(tx as f32);
                }
            }
            self.prev
                .insert(iface.name.clone(), (iface.rx_bytes, iface.tx_bytes, now));
        }
    }
}

/// What is playing, and where in it.
pub struct MediaStore {
    /// `None` means no player is present.
    pub latest: Option<Media>,
    pub error: Option<String>,
    /// When `latest` arrived, used to interpolate the position during playback.
    at: Instant,
    /// The last track that came with art, and that art.
    last_art: Option<(Media, String)>,
}

impl Default for MediaStore {
    fn default() -> Self {
        Self {
            latest: None,
            error: None,
            at: Instant::now(),
            last_art: None,
        }
    }
}

/// Whether two readings are the same track, whatever the position, status or art.
fn same_track(a: &Media, b: &Media) -> bool {
    a.player == b.player
        && a.title == b.title
        && a.artist == b.artist
        && a.album == b.album
        && a.length_us == b.length_us
}

impl MediaStore {
    pub fn apply(&mut self, sample: MediaSample) -> bool {
        match sample {
            MediaSample::State(mut media) => {
                // Firefox drops the art of a track it comes back to from another tab's
                // media, though the track is the same. The art it last gave for that
                // track stands in, or the frame would stay empty until the next track.
                if let Some(media) = media.as_mut() {
                    if !media.art_url.is_empty() {
                        self.last_art = Some((media.clone(), media.art_url.clone()));
                    } else if let Some((track, url)) = &self.last_art
                        && same_track(track, media)
                    {
                        media.art_url = url.clone();
                    }
                }
                let changed = self.latest != media;
                self.latest = media;
                self.at = Instant::now();
                self.error = None;
                changed
            }
            MediaSample::Failed(message) => {
                self.error = Some(message);
                true
            }
        }
    }

    /// The playback position to display, in microseconds.
    ///
    /// `playerctl -F` does not tick the position out during playback.
    /// While playing, **interpolate by adding the time elapsed since the last update**.
    pub fn position_us(&self) -> u64 {
        let Some(media) = &self.latest else {
            return 0;
        };
        if media.status != Status::Playing {
            return media.position_us;
        }
        let elapsed = self.at.elapsed().as_micros() as u64;
        let pos = media.position_us.saturating_add(elapsed);
        if media.length_us > 0 {
            pos.min(media.length_us)
        } else {
            pos
        }
    }
}

/// What the panel itself is doing, as opposed to what it is showing.
#[derive(Default)]
pub struct UiState {
    /// A power action awaiting confirmation. While `Some`, the modal is up and swallows
    /// key input.
    pub pending_power: Option<PowerAction>,
    /// The action pending execution during the delay, kept so the UI can say it is coming.
    pub power_running: Option<PowerAction>,
    pub power_error: Option<String>,
    /// Whether Yes has focus in the modal. **It defaults to No.**
    pub confirm_yes_focused: bool,
    /// Whether the help overlay is showing.
    pub show_help: bool,
    /// The rect-to-action table built by the most recent draw.
    pub hits: HitMap,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str, art_url: &str) -> Option<Media> {
        Some(Media {
            player: "firefox".into(),
            status: Status::Playing,
            position_us: 0,
            length_us: 247_000_000,
            art_url: art_url.into(),
            title: title.into(),
            artist: "Sample Artist".into(),
            album: "Sample Album".into(),
        })
    }

    fn art(store: &MediaStore) -> &str {
        store
            .latest
            .as_ref()
            .map_or("", |media| media.art_url.as_str())
    }

    /// A track that comes back without its art gets the art it last had, even with
    /// other media that had none in between.
    #[test]
    fn a_track_that_comes_back_without_art_keeps_the_art_it_had() {
        let mut store = MediaStore::default();
        store.apply(MediaSample::State(track("One", "file:///tmp/one.png")));
        store.apply(MediaSample::State(track("A video", "")));
        assert_eq!(art(&store), "");

        store.apply(MediaSample::State(track("One", "")));
        assert_eq!(art(&store), "file:///tmp/one.png");
    }

    /// Only the same track borrows art, and art the player gives always wins.
    #[test]
    fn another_track_never_borrows_art() {
        let mut store = MediaStore::default();
        store.apply(MediaSample::State(track("One", "file:///tmp/one.png")));
        store.apply(MediaSample::State(track("Two", "")));
        assert_eq!(art(&store), "");

        store.apply(MediaSample::State(track("One", "file:///tmp/new.png")));
        assert_eq!(art(&store), "file:///tmp/new.png");
    }
}
