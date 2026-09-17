//! The Hyprland workspace display.
//!
//! - State is queried straight from `.socket.sock` (`hyprctl` is never spawned)
//! - Changes arrive as pushes on `.socket2.sock` (no polling)
//!
//! socket2 has many event kinds, and accumulating diffs by hand risks missing one.
//! **Every event re-reads the whole state instead,** favouring correctness.

use std::collections::BTreeMap;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::config::{CommandsConfig, WorkspacesConfig};
use crate::sources::provider::{
    COALESCE_WINDOW, Coalesce, Fold, ProviderHandle, STOP_CHECK_INTERVAL, Shutdown,
    wait_for_commands,
};

/// The workspace row for one monitor.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorRow {
    pub name: String,
    pub active: i64,
    /// The monitor holding the keyboard focus. Exactly one is expected, but Hyprland
    /// is free to report none, so the display must not rely on it being present.
    pub focused: bool,
    pub workspaces: Vec<WorkspaceCell>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceCell {
    pub id: i64,
    /// Holds at least one window.
    pub occupied: bool,
    /// Where the windows sit, as fractions of their monitor. The display draws these
    /// as a mini-map, so the geometry is normalised here and the UI stays pure.
    pub tiles: Vec<Tile>,
}

/// One window, as a fraction of its monitor: `0.0..=1.0` on both axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tile {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Workspaces {
    pub monitors: Vec<MonitorRow>,
}

// ---- Hyprland's JSON ----

#[derive(Debug, Deserialize)]
struct RawMonitor {
    name: String,
    #[serde(rename = "activeWorkspace")]
    active_workspace: RawActive,
    /// Older Hyprland builds may not send this; missing means "not focused".
    #[serde(default)]
    focused: bool,
    /// Layout position and size. `width`/`height` are physical pixels, so the logical
    /// size the client coordinates use is these divided by `scale`.
    #[serde(default)]
    x: i32,
    #[serde(default)]
    y: i32,
    #[serde(default)]
    width: f32,
    #[serde(default)]
    height: f32,
    #[serde(default = "one")]
    scale: f32,
    /// Rotation. The odd values are the quarter turns, and for those `width` and
    /// `height` - which describe the panel, not the layout - are the wrong way round.
    #[serde(default)]
    transform: i32,
}

fn one() -> f32 {
    1.0
}

#[derive(Debug, Deserialize)]
struct RawClient {
    /// `[x, y]` in layout coordinates.
    #[serde(default)]
    at: [i32; 2],
    /// `[width, height]`, logical.
    #[serde(default)]
    size: [i32; 2],
    workspace: RawActive,
    /// An unmapped client has no place on screen yet.
    #[serde(default = "yes")]
    mapped: bool,
    #[serde(default)]
    hidden: bool,
}

fn yes() -> bool {
    true
}

/// The logical rectangle a monitor occupies in layout coordinates.
#[derive(Debug, Clone, Copy)]
struct MonitorRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

#[derive(Debug, Deserialize)]
struct RawActive {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct RawWorkspace {
    id: i64,
    monitor: String,
    #[serde(default)]
    windows: i64,
}

/// The directory holding the sockets.
fn socket_dir() -> Result<PathBuf, String> {
    let runtime =
        std::env::var("XDG_RUNTIME_DIR").map_err(|_| "XDG_RUNTIME_DIR is not set".to_string())?;
    let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").map_err(|_| {
        "HYPRLAND_INSTANCE_SIGNATURE is not set (not running under Hyprland?)".to_string()
    })?;
    Ok(PathBuf::from(runtime).join("hypr").join(signature))
}

/// Send a command to `.socket.sock` and read the whole reply.
///
/// `timeout` always applies, so a stuck Hyprland cannot stall the event loop.
pub fn request(command: &str, timeout: Duration) -> Result<String, String> {
    let path = socket_dir()?.join(".socket.sock");
    let stream =
        UnixStream::connect(&path).map_err(|e| format!("connect {}: {e}", path.display()))?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|e| format!("set timeout on {}: {e}", path.display()))?;
    let mut stream = stream;
    stream
        .write_all(command.as_bytes())
        .map_err(|e| format!("write {command}: {e}"))?;
    let mut buf = String::new();
    stream
        .read_to_string(&mut buf)
        .map_err(|e| format!("read {command}: {e}"))?;
    Ok(buf)
}

/// Build the display structure from the JSON documents already fetched.
///
/// Parsing is a pure function, so it can be tested against fixtures.
pub fn parse(
    monitors_json: &str,
    workspaces_json: &str,
    clients_json: &str,
) -> Result<Workspaces, String> {
    let monitors: Vec<RawMonitor> =
        serde_json::from_str(monitors_json).map_err(|e| format!("monitors: {e}"))?;
    let workspaces: Vec<RawWorkspace> =
        serde_json::from_str(workspaces_json).map_err(|e| format!("workspaces: {e}"))?;
    // The mini-map is a nicety: a clients document we cannot read costs the tiles,
    // not the whole workspace row.
    let clients: Vec<RawClient> = serde_json::from_str(clients_json).unwrap_or_default();

    let rects: BTreeMap<&str, MonitorRect> = monitors
        .iter()
        .map(|m| {
            let scale = if m.scale > 0.0 { m.scale } else { 1.0 };
            let (width, height) = if m.transform % 2 == 0 {
                (m.width, m.height)
            } else {
                (m.height, m.width)
            };
            (
                m.name.as_str(),
                MonitorRect {
                    x: m.x as f32,
                    y: m.y as f32,
                    width: width / scale,
                    height: height / scale,
                },
            )
        })
        .collect();

    // Which monitor each workspace lives on, so a client can be placed against it.
    let homes: BTreeMap<i64, &str> = workspaces
        .iter()
        .map(|w| (w.id, w.monitor.as_str()))
        .collect();

    let mut tiles: BTreeMap<i64, Vec<Tile>> = BTreeMap::new();
    for client in &clients {
        if !client.mapped || client.hidden {
            continue;
        }
        let id = client.workspace.id;
        let Some(rect) = homes.get(&id).and_then(|name| rects.get(name)) else {
            continue;
        };
        if rect.width <= 0.0 || rect.height <= 0.0 {
            continue;
        }
        tiles.entry(id).or_default().push(Tile {
            x: (client.at[0] as f32 - rect.x) / rect.width,
            y: (client.at[1] as f32 - rect.y) / rect.height,
            width: client.size[0] as f32 / rect.width,
            height: client.size[1] as f32 / rect.height,
        });
    }

    let rows = monitors
        .into_iter()
        .map(|monitor| {
            let mut cells: Vec<WorkspaceCell> = workspaces
                .iter()
                .filter(|w| w.monitor == monitor.name)
                // Special workspaces arrive with a negative id. They clutter the row, so they are dropped.
                .filter(|w| w.id >= 0)
                .map(|w| WorkspaceCell {
                    id: w.id,
                    occupied: w.windows > 0,
                    tiles: tiles.get(&w.id).cloned().unwrap_or_default(),
                })
                .collect();
            cells.sort_by_key(|c| c.id);
            MonitorRow {
                name: monitor.name,
                active: monitor.active_workspace.id,
                focused: monitor.focused,
                workspaces: cells,
            }
        })
        .collect();

    Ok(Workspaces { monitors: rows })
}

/// Re-read the entire current state.
pub fn read(commands: &CommandsConfig) -> Result<Workspaces, String> {
    let timeout = commands.hyprland_timeout();
    let monitors = request("j/monitors", timeout)?;
    let workspaces = request("j/workspaces", timeout)?;
    // A third request, on the same socket and still without spawning anything.
    let clients = request("j/clients", timeout).unwrap_or_default();
    parse(&monitors, &workspaces, &clients)
}

/// Switch to a workspace.
///
/// This goes down the same `.socket.sock` the state is read from, so a click costs
/// no process either. What follows `dispatch ` is configurable because
/// the syntax is not the same on every build: upstream takes `workspace 3`, and other
/// builds may take an expression instead.
///
/// No refresh is triggered here - the switch makes Hyprland emit a `workspace` event
/// on socket2, and the existing subscription is what redraws.
pub fn dispatch_workspace(
    id: i64,
    workspaces: &WorkspacesConfig,
    commands: &CommandsConfig,
) -> Result<(), String> {
    let reply = request(
        &switch_command(&workspaces.switch, id),
        commands.hyprland_timeout(),
    )?;
    // Hyprland answers `ok` on success and a message on failure; taking the reply on
    // trust would make a workspace that refused to switch look like one that did.
    if reply.trim() == "ok" {
        return Ok(());
    }
    let message = reply.lines().next().unwrap_or("").trim();
    Err(if message.is_empty() {
        "no reply".to_string()
    } else {
        message.to_string()
    })
}

/// The line sent down the socket for a switch.
fn switch_command(template: &str, id: i64) -> String {
    format!("dispatch {}", template.replace("{id}", &id.to_string()))
}

/// Subscribe to `.socket2.sock` and call `on_notice` for every event the coordinator
/// acts on.
///
/// **This function blocks. Call it from a dedicated thread.**
/// It returns `Ok` when `cancel` says to stop, and `Err` on disconnect so the caller can
/// back off and reconnect. The read timeout is what lets it notice either one.
pub fn subscribe(
    cancel: &dyn Fn() -> bool,
    mut on_notice: impl FnMut(HyprlandCommand),
) -> Result<(), String> {
    let path = socket_dir()?.join(".socket2.sock");
    let mut stream =
        UnixStream::connect(&path).map_err(|e| format!("connect {}: {e}", path.display()))?;
    stream
        .set_read_timeout(Some(STOP_CHECK_INTERVAL))
        .map_err(|e| format!("socket2 read timeout: {e}"))?;

    // Lines are assembled here rather than by `BufReader::lines`, which cannot tell a
    // read timeout from the end of the stream.
    let mut pending = String::new();
    let mut buf = [0u8; 4096];
    loop {
        if cancel() {
            return Ok(());
        }
        match stream.read(&mut buf) {
            Ok(0) => return Err("socket2 closed".to_string()),
            Ok(n) => {
                pending.push_str(&String::from_utf8_lossy(&buf[..n]));
                while let Some(at) = pending.find('\n') {
                    let line: String = pending.drain(..=at).collect();
                    if let Some(command) = notice(line.trim_end()) {
                        on_notice(command);
                    }
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => continue,
            Err(e) => return Err(format!("read socket2: {e}")),
        }
    }
}

/// What a socket2 line asks of the coordinator, if anything.
fn notice(line: &str) -> Option<HyprlandCommand> {
    if affects_workspaces(line) {
        return Some(HyprlandCommand::Changed);
    }
    let (event, data) = line.split_once(">>")?;
    (event == "activewindowv2").then(|| HyprlandCommand::Focus(data.to_string()))
}

/// socket2 lines are `event>>data`. Only the ones affecting the workspace display are picked up.
fn affects_workspaces(line: &str) -> bool {
    let event = line.split(">>").next().unwrap_or("");
    matches!(
        event,
        "workspace"
            | "workspacev2"
            | "focusedmon"
            | "focusedmonv2"
            | "createworkspace"
            | "createworkspacev2"
            | "destroyworkspace"
            | "destroyworkspacev2"
            | "moveworkspace"
            | "moveworkspacev2"
            | "openwindow"
            | "closewindow"
            | "movewindow"
            | "movewindowv2"
            | "monitoradded"
            | "monitorremoved"
    )
}

// ---- Provider ----

/// One sample from the Hyprland provider.
#[derive(Debug, Clone)]
pub enum HyprlandSample {
    State(Result<Workspaces, String>),
    /// A switch was refused. The workspaces on screen are still the real ones.
    SwitchFailed(String),
    /// The active window changed, by address. Only reported when the provider is asked to.
    Focus(String),
}

/// What reaches the Hyprland coordinator.
///
/// `Changed` and `Focus` come from the reader thread watching socket2; the rest come
/// from the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HyprlandCommand {
    Switch(i64),
    Refresh,
    Changed,
    Focus(String),
}

#[derive(PartialEq)]
pub enum HyprlandKey {
    Switch,
    Read,
    Focus,
}

impl Coalesce for HyprlandCommand {
    type Key = HyprlandKey;

    fn key(&self) -> HyprlandKey {
        match self {
            HyprlandCommand::Switch(_) => HyprlandKey::Switch,
            HyprlandCommand::Refresh | HyprlandCommand::Changed => HyprlandKey::Read,
            HyprlandCommand::Focus(_) => HyprlandKey::Focus,
        }
    }

    fn fold(&mut self, next: &Self) -> Fold {
        match (&*self, next) {
            // A burst of socket2 events is one re-read, which is what keeps a workspace
            // storm from turning into a queue of identical reads.
            (_, HyprlandCommand::Changed) | (_, HyprlandCommand::Refresh) => Fold::Merged,
            // Clicking two workspaces in one window means the last one wins.
            (HyprlandCommand::Switch(_), HyprlandCommand::Switch(_))
            // Focus passing through three windows in a burst is focus on the third.
            | (HyprlandCommand::Focus(_), HyprlandCommand::Focus(_)) => {
                *self = next.clone();
                Fold::Merged
            }
            _ => Fold::Keep,
        }
    }
}

/// Watches socket2 on a reader thread and answers switches on the coordinator.
pub struct HyprlandProvider {
    pub workspaces: WorkspacesConfig,
    pub commands: CommandsConfig,
    /// Report the active window as it changes. Off, focus changes cost nothing.
    pub focus: bool,
}

impl HyprlandProvider {
    pub fn spawn(
        self,
        samples: calloop::channel::Sender<HyprlandSample>,
    ) -> Result<ProviderHandle<HyprlandCommand>, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let shutdown = Shutdown::new();
        let stop = shutdown.clone();
        let notices = tx.clone();

        // The reader reports; it never builds a sample of its own.
        let reader_stop = shutdown.clone();
        let focus = self.focus;
        let reader = std::thread::Builder::new()
            .name("pippipit-hypr-rd".into())
            .spawn(move || {
                let mut backoff = Duration::from_secs(1);
                while !reader_stop.is_requested() {
                    // Always fire once on connecting, to pick up whatever was missed.
                    if notices.send(HyprlandCommand::Changed).is_err() {
                        return;
                    }
                    let mut heard = false;
                    let result = subscribe(&|| reader_stop.is_requested(), |command| {
                        heard = true;
                        if focus || !matches!(command, HyprlandCommand::Focus(_)) {
                            let _ = notices.send(command);
                        }
                    });
                    if result.is_ok() || heard {
                        backoff = Duration::from_secs(1);
                    }
                    if !reader_stop.sleep(backoff) {
                        return;
                    }
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            })
            .map_err(|e| format!("failed to spawn the hyprland reader: {e}"))?;

        // Reachable from the coordinator and from the failure path below, so whichever
        // gets there first is the one that waits for the reader.
        let reader = std::sync::Arc::new(std::sync::Mutex::new(Some(reader)));
        let reader_for_coordinator = std::sync::Arc::clone(&reader);
        let coordinator_stop = shutdown.clone();

        let join = std::thread::Builder::new()
            .name("pippipit-hypr".into())
            .spawn(move || {
                'coordinate: loop {
                    let deadline = Instant::now() + IDLE_WAIT;
                    let Some(commands) = wait_for_commands(&rx, &stop, deadline, COALESCE_WINDOW)
                    else {
                        break;
                    };
                    for command in commands {
                        match command {
                            HyprlandCommand::Switch(id) => {
                                if let Err(message) =
                                    dispatch_workspace(id, &self.workspaces, &self.commands)
                                {
                                    let failed = format!("workspace {id}: {message}");
                                    if samples.send(HyprlandSample::SwitchFailed(failed)).is_err() {
                                        break 'coordinate; // the event loop is gone
                                    }
                                }
                            }
                            HyprlandCommand::Refresh | HyprlandCommand::Changed => {
                                let state = read(&self.commands);
                                if samples.send(HyprlandSample::State(state)).is_err() {
                                    break 'coordinate;
                                }
                            }
                            HyprlandCommand::Focus(address) => {
                                if samples.send(HyprlandSample::Focus(address)).is_err() {
                                    break 'coordinate;
                                }
                            }
                        }
                    }
                }
                // Every way out of the loop comes through here: the reader outlives the
                // coordinator otherwise, watching a socket for nobody.
                coordinator_stop.request();
                join_reader(&reader_for_coordinator);
            });
        let join = match join {
            Ok(join) => join,
            Err(e) => {
                shutdown.request();
                join_reader(&reader);
                return Err(format!("failed to spawn the hyprland thread: {e}"));
            }
        };
        Ok(ProviderHandle::new(tx, shutdown, join))
    }
}

/// Take the reader thread out of the shared slot and wait for it.
fn join_reader(slot: &std::sync::Mutex<Option<std::thread::JoinHandle<()>>>) {
    let reader = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(reader) = reader {
        let _ = reader.join();
    }
}

/// The longest gap between reconnection attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// How long the coordinator waits when nothing at all is happening.
const IDLE_WAIT: Duration = Duration::from_secs(3600);

#[cfg(test)]
mod tests {
    use super::*;

    /// A burst of socket2 events must cost one re-read, not one per event.
    ///
    /// One user action can make socket2 emit several events, and a single dispatch can
    /// deliver up to 1024 of them.
    #[test]
    fn a_storm_of_socket_events_becomes_one_read() {
        let storm = vec![HyprlandCommand::Changed; 1000];
        assert_eq!(
            crate::sources::provider::coalesce(storm),
            vec![HyprlandCommand::Changed]
        );
    }

    /// Two clicks in one window switch once, to the workspace clicked last.
    #[test]
    fn only_the_last_switch_in_a_burst_runs() {
        let burst = vec![HyprlandCommand::Switch(3), HyprlandCommand::Switch(5)];
        assert_eq!(
            crate::sources::provider::coalesce(burst),
            vec![HyprlandCommand::Switch(5)]
        );
    }

    /// A switch and the events it causes stay in order: switch first, then the re-read.
    #[test]
    fn a_switch_is_followed_by_its_read() {
        let burst = vec![
            HyprlandCommand::Switch(3),
            HyprlandCommand::Changed,
            HyprlandCommand::Changed,
        ];
        assert_eq!(
            crate::sources::provider::coalesce(burst),
            vec![HyprlandCommand::Switch(3), HyprlandCommand::Changed]
        );
    }

    /// The read a switch causes must not be folded into the read that came before it:
    /// that would leave the pane showing the workspace it just left.
    #[test]
    fn a_read_before_a_switch_does_not_swallow_the_one_after() {
        let burst = vec![
            HyprlandCommand::Changed,
            HyprlandCommand::Switch(3),
            HyprlandCommand::Changed,
        ];
        assert_eq!(
            crate::sources::provider::coalesce(burst),
            vec![
                HyprlandCommand::Changed,
                HyprlandCommand::Switch(3),
                HyprlandCommand::Changed
            ]
        );
    }

    const MONITORS: &str = r#"[
        {"name":"DP-1","activeWorkspace":{"id":2,"name":"2"},"focused":true,
         "x":0,"y":0,"width":2560,"height":1440,"scale":1.0},
        {"name":"DP-2","activeWorkspace":{"id":10,"name":"10"},
         "x":2560,"y":0,"width":1920,"height":1080,"scale":1.0},
        {"name":"HDMI-A-1","activeWorkspace":{"id":9,"name":"9"},
         "x":4480,"y":0,"width":1920,"height":1080,"scale":1.0}
    ]"#;
    const WORKSPACES: &str = r#"[
        {"id":9,"monitor":"HDMI-A-1","windows":3},
        {"id":1,"monitor":"DP-1","windows":1},
        {"id":10,"monitor":"DP-2","windows":2},
        {"id":2,"monitor":"DP-1","windows":0}
    ]"#;

    /// Two windows split left and right on DP-1's workspace 1, and one filling
    /// workspace 10 over on DP-2 - which starts at x=2560, so its coordinates
    /// only normalise correctly if the monitor offset is subtracted.
    const CLIENTS: &str = r#"[
        {"at":[0,0],"size":[1280,1440],"workspace":{"id":1,"name":"1"}},
        {"at":[1280,0],"size":[1280,1440],"workspace":{"id":1,"name":"1"}},
        {"at":[2560,0],"size":[1920,1080],"workspace":{"id":10,"name":"10"}}
    ]"#;

    #[test]
    fn groups_workspaces_by_monitor_in_monitor_order() {
        let w = parse(MONITORS, WORKSPACES, "[]").unwrap();
        assert_eq!(
            w.monitors
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            ["DP-1", "DP-2", "HDMI-A-1"]
        );
        assert_eq!(
            w.monitors[0].workspaces,
            vec![
                WorkspaceCell {
                    id: 1,
                    occupied: true,
                    tiles: vec![]
                },
                WorkspaceCell {
                    id: 2,
                    occupied: false,
                    tiles: vec![]
                },
            ]
        );
        assert_eq!(w.monitors[0].active, 2);
    }

    #[test]
    fn sorts_by_id_regardless_of_input_order() {
        let ids: Vec<i64> = parse(MONITORS, WORKSPACES, "[]").unwrap().monitors[0]
            .workspaces
            .iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ids, [1, 2]);
    }

    /// Special workspaces (negative ids) never reach the row.
    #[test]
    fn hides_special_workspaces() {
        let ws =
            r#"[{"id":-99,"monitor":"DP-1","windows":1},{"id":1,"monitor":"DP-1","windows":0}]"#;
        let w = parse(MONITORS, ws, "[]").unwrap();
        assert_eq!(w.monitors[0].workspaces.len(), 1);
        assert_eq!(w.monitors[0].workspaces[0].id, 1);
    }

    #[test]
    fn monitor_without_workspaces_still_gets_a_row() {
        let w = parse(MONITORS, "[]", "[]").unwrap();
        assert_eq!(w.monitors.len(), 3);
        assert!(w.monitors.iter().all(|m| m.workspaces.is_empty()));
    }

    /// The focused monitor drives the workspace row, so it must survive parsing -
    /// and its absence must not be an error.
    #[test]
    fn reads_the_focused_monitor() {
        let w = parse(MONITORS, WORKSPACES, "[]").unwrap();
        assert!(w.monitors[0].focused);
        assert!(!w.monitors[1].focused);

        let none = r#"[{"name":"DP-1","activeWorkspace":{"id":1,"name":"1"}}]"#;
        assert!(!parse(none, "[]", "[]").unwrap().monitors[0].focused);
    }

    /// The mini-map needs the windows as fractions of **their own** monitor.
    #[test]
    fn clients_become_tiles_normalised_to_their_monitor() {
        let w = parse(MONITORS, WORKSPACES, CLIENTS).unwrap();
        let ws1 = &w.monitors[0].workspaces[0];
        assert_eq!(ws1.id, 1);
        assert_eq!(ws1.tiles.len(), 2);
        assert_eq!(
            ws1.tiles[0],
            Tile {
                x: 0.0,
                y: 0.0,
                width: 0.5,
                height: 1.0
            }
        );
        assert_eq!(
            ws1.tiles[1],
            Tile {
                x: 0.5,
                y: 0.0,
                width: 0.5,
                height: 1.0
            }
        );

        // Workspace 10 lives on DP-2, which starts at x=2560.
        let ws10 = &w.monitors[1].workspaces[0];
        assert_eq!(ws10.id, 10);
        assert_eq!(
            ws10.tiles[0],
            Tile {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0
            },
            "the monitor offset has to come off first"
        );
    }

    /// A quarter-turned monitor reports the panel's dimensions, not the layout's.
    /// Getting this wrong makes every window on that monitor look too narrow.
    #[test]
    fn a_rotated_monitor_swaps_width_and_height() {
        let monitors = r#"[
            {"name":"DP-2","activeWorkspace":{"id":10,"name":"10"},"focused":true,
             "x":-1000,"y":0,"width":2560,"height":1600,"scale":1.6,"transform":1}
        ]"#;
        let workspaces = r#"[{"id":10,"monitor":"DP-2","windows":1}]"#;
        // Full width of the 1000x1600 logical monitor.
        let clients = r#"[{"at":[-1000,0],"size":[1000,1600],"workspace":{"id":10,"name":"10"}}]"#;
        let w = parse(monitors, workspaces, clients).unwrap();
        assert_eq!(
            w.monitors[0].workspaces[0].tiles[0],
            Tile {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0
            }
        );
    }

    /// A window that is not on screen has no place on the mini-map.
    #[test]
    fn unmapped_and_hidden_clients_are_dropped() {
        let clients = r#"[
            {"at":[0,0],"size":[100,100],"workspace":{"id":1,"name":"1"},"mapped":false},
            {"at":[0,0],"size":[100,100],"workspace":{"id":1,"name":"1"},"hidden":true}
        ]"#;
        let w = parse(MONITORS, WORKSPACES, clients).unwrap();
        assert!(w.monitors[0].workspaces[0].tiles.is_empty());
    }

    /// The tiles are a nicety; losing them must not lose the workspaces.
    #[test]
    fn broken_clients_json_still_yields_workspaces() {
        let w = parse(MONITORS, WORKSPACES, "not json").unwrap();
        assert_eq!(w.monitors.len(), 3);
        assert!(w.monitors[0].workspaces.iter().all(|c| c.tiles.is_empty()));
    }

    #[test]
    fn broken_json_is_an_error() {
        assert!(parse("{", "[]", "[]").is_err());
        assert!(parse("[]", "nope", "[]").is_err());
    }

    /// The active window is the art's business, not the workspace row's: it must not
    /// cost a re-read.
    #[test]
    fn the_active_window_is_a_notice_of_its_own() {
        assert_eq!(
            notice("activewindowv2>>55d3b1c0a2f0"),
            Some(HyprlandCommand::Focus("55d3b1c0a2f0".into()))
        );
        assert_eq!(notice("workspace>>3"), Some(HyprlandCommand::Changed));
        assert_eq!(notice("activewindow>>Alacritty,foo"), None);
        assert_eq!(notice(""), None);
    }

    #[test]
    fn focus_changes_in_a_burst_land_on_the_last_window() {
        let burst = vec![
            HyprlandCommand::Focus("a".into()),
            HyprlandCommand::Focus("b".into()),
        ];
        assert_eq!(
            crate::sources::provider::coalesce(burst),
            vec![HyprlandCommand::Focus("b".into())]
        );
    }

    #[test]
    fn picks_up_the_events_that_matter() {
        for line in [
            "workspace>>3",
            "workspacev2>>3,3",
            "focusedmon>>DP-1,2",
            "openwindow>>abc,1,foo,bar",
            "closewindow>>abc",
            "destroyworkspacev2>>4,4",
            "monitoradded>>DP-3",
        ] {
            assert!(affects_workspaces(line), "should react to {line}");
        }
    }

    #[test]
    fn ignores_noisy_events() {
        for line in [
            "activewindow>>Alacritty,foo",
            "activewindowv2>>abc",
            "activelayout>>keyboard,jp",
            "submap>>resize",
            "configreloaded>>",
            "",
        ] {
            assert!(!affects_workspaces(line), "should ignore {line}");
        }
    }

    /// The default is upstream's syntax; an override may be an expression, and the substitution
    /// has to survive the braces such an expression carries.
    #[test]
    fn the_switch_command_takes_the_id_from_the_template() {
        let default = WorkspacesConfig::default();
        assert_eq!(switch_command(&default.switch, 3), "dispatch workspace 3");
        assert_eq!(
            switch_command("hl.dsp.focus({ workspace = {id} })", 10),
            "dispatch hl.dsp.focus({ workspace = 10 })"
        );
    }
}
