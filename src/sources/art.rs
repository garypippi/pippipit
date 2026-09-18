//! Album art, laid over the pane by a resident Überzug++.
//!
//! In a terminal with no image protocol, Überzug++ draws on Wayland: every image is a
//! window of its own, moved over the terminal. **On Hyprland it takes the terminal to be
//! whichever window is active at the moment it starts**, and follows that window for the
//! rest of its life. A layer started while another window has focus lays the art over
//! that window instead. So on Wayland the layer is only started while pippipit's own
//! window is the active one, and a layer that dies waits for the next time it is.
//!
//! Each image opens on the workspace that has focus when it is added. Keeping it on the
//! pane's workspace takes a window rule on titles starting `ueberzugpp_`; nothing on this
//! side can do that.
//!
//! Überzug++ also moves each window over the terminal itself, but in upstream dispatch
//! syntax and with a scale correction that only holds for a whole-number scale. So once
//! the window has opened, pippipit moves it over the frame again, through
//! `[art] move`. The image shows where it opened until then; a window rule that opens it
//! transparent hides that, and `[art] show` makes it visible once it is in place.
//!
//! A layer reads its commands from stdin and exits when stdin closes, so a panel that
//! dies takes its art with it.

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::config::{ArtConfig, CommandsConfig};
use crate::sources::hyprland;
use crate::sources::media::Media;
use crate::sources::provider::{
    COALESCE_WINDOW, Coalesce, CommandRunner, Fold, ProviderHandle, Shutdown, SystemRunner,
    wait_for_commands,
};
use crate::util::proc;

/// The one image the panel shows. Adding under the same name replaces it.
const IDENTIFIER: &str = "pippipit-art";

/// How many downloaded covers the cache keeps. Past this the oldest go.
const CACHE_KEEP: usize = 64;

/// How long the coordinator waits when nothing at all is happening.
const IDLE_WAIT: Duration = Duration::from_secs(3600);

/// How long an image window has to turn up after the add before it is left where it
/// opened, and how often to look for it in the meantime.
const WINDOW_WAIT: Duration = Duration::from_secs(2);
const WINDOW_POLL: Duration = Duration::from_millis(50);

/// Where the art goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// `mpris:artUrl`, as the player gave it.
    pub url: String,
    /// Cells, counted from the terminal's top-left corner.
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    /// Columns and rows of the terminal. Überzug++ only moves its window when it draws,
    /// so a resize that leaves the cells where they were still has to add the image again.
    pub terminal: (u16, u16),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ArtCommand {
    Show(Placement),
    Hide,
    /// The active window changed, by address, as socket2's `activewindowv2` gives it.
    Focus(String),
}

#[derive(PartialEq)]
pub enum ArtKey {
    Placement,
    Focus,
}

impl Coalesce for ArtCommand {
    type Key = ArtKey;

    fn key(&self) -> ArtKey {
        match self {
            ArtCommand::Focus(_) => ArtKey::Focus,
            ArtCommand::Show(_) | ArtCommand::Hide => ArtKey::Placement,
        }
    }

    /// Where the art goes and which window has focus are states, not steps: only the
    /// newest of each is worth acting on.
    fn fold(&mut self, next: &Self) -> Fold {
        *self = next.clone();
        Fold::Merged
    }

    fn commutes_with(&self, next: &Self) -> bool {
        self.key() != next.key()
    }
}

/// Stands in for an art URL when the player is cmus, which names no art over mpris.
const CMUS_SCHEME: &str = "cmus://";

/// The names a cover beside a song file goes by, in the order they are looked for.
const SIDE_COVERS: [&str; 6] = [
    "cover.jpg",
    "cover.png",
    "folder.jpg",
    "folder.png",
    "front.jpg",
    "front.png",
];

/// What identifies a track's art: `mpris:artUrl`, when the player gives one.
///
/// cmus gives none, and names no file either, so for cmus it is the track itself under a
/// scheme of pippipit's own. A new track makes a new one, which is what sends the
/// provider to ask cmus which file is playing.
pub fn url_for(media: &Media) -> Option<String> {
    if !media.art_url.is_empty() {
        return Some(media.art_url.clone());
    }
    (media.player == "cmus").then(|| {
        format!(
            "{CMUS_SCHEME}{}\t{}\t{}\t{}",
            media.title, media.artist, media.album, media.length_us
        )
    })
}

/// Where an art URL leads.
#[derive(Debug, PartialEq)]
pub enum Source {
    File(PathBuf),
    Remote(String),
    /// Whatever cmus is playing: the cover is in or beside that file.
    Cmus,
}

/// `None` for anything that is neither a local path, something `curl` can fetch, nor
/// cmus's stand-in.
pub fn source(url: &str) -> Option<Source> {
    let url = url.trim();
    if url.starts_with(CMUS_SCHEME) {
        return Some(Source::Cmus);
    }
    if let Some(rest) = url.strip_prefix("file://") {
        let path = percent_decode(rest)?;
        return path
            .starts_with('/')
            .then(|| Source::File(PathBuf::from(path)));
    }
    if url.starts_with("https://") || url.starts_with("http://") {
        return Some(Source::Remote(url.to_string()));
    }
    None
}

/// `%20` and friends back into bytes. A malformed escape makes the whole URL unusable.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The file a URL is cached under. FNV-1a, so the name stays the same across builds and
/// a restart finds what the last run downloaded.
fn cache_name(url: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in url.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(base.join("pippipit").join("art"))
}

/// Download a cover into `dir`, or find it there from before.
///
/// It lands under a `.part` name first, so a download cut off halfway is never taken
/// for a cover on the next look.
fn fetch(url: &str, dir: &Path, curl: &str, runner: &dyn CommandRunner) -> Result<PathBuf, String> {
    let path = dir.join(cache_name(url));
    if path.is_file() {
        return Ok(path);
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let part = path.with_extension("part");
    let part_arg = part.to_string_lossy();
    if let Err(message) = runner.status(
        curl,
        &["-fsSL", "--max-filesize", "16M", "-o", &part_arg, url],
    ) {
        let _ = std::fs::remove_file(&part);
        return Err(message);
    }
    std::fs::rename(&part, &path).map_err(|e| format!("rename {}: {e}", part.display()))?;
    prune(dir, CACHE_KEEP);
    Ok(path)
}

/// The file from `cmus-remote -Q`, whose line for it reads `file /path/to/song.flac`.
fn cmus_file(status: &str) -> Option<PathBuf> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("file "))
        .map(PathBuf::from)
}

/// A song file's cover: the one embedded in it, taken out into `dir`, or else an image
/// beside it.
///
/// Taken out once per version of the file: the cache name comes from the path and the
/// time the file was last written.
fn local_cover(
    song: &Path,
    dir: &Path,
    ffmpeg: &str,
    runner: &dyn CommandRunner,
) -> Option<PathBuf> {
    let written = song.metadata().and_then(|m| m.modified()).ok()?;
    let stamp = written
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or(0);
    let path = dir.join(cache_name(&format!("{}\t{stamp}", song.display())));
    if path.is_file() {
        return Some(path);
    }
    if std::fs::create_dir_all(dir).is_ok() {
        let part = path.with_extension("part");
        let (song_arg, part_arg) = (song.to_string_lossy(), part.to_string_lossy());
        // An embedded cover is a video stream of one frame. It is copied out as it is,
        // and a song without one makes ffmpeg fail on the map.
        let taken = runner
            .status(
                ffmpeg,
                &[
                    "-v",
                    "error",
                    "-y",
                    "-i",
                    &song_arg,
                    "-an",
                    "-map",
                    "0:v:0",
                    "-frames:v",
                    "1",
                    "-c",
                    "copy",
                    "-f",
                    "image2",
                    "-update",
                    "1",
                    &part_arg,
                ],
            )
            .is_ok()
            && part.metadata().is_ok_and(|m| m.len() > 0);
        if taken && std::fs::rename(&part, &path).is_ok() {
            prune(dir, CACHE_KEEP);
            return Some(path);
        }
        let _ = std::fs::remove_file(&part);
    }
    let folder = song.parent()?;
    SIDE_COVERS
        .iter()
        .map(|name| folder.join(name))
        .find(|beside| beside.is_file())
}

/// Keep the newest `keep` covers and delete the rest.
fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut covers: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_none())
        .filter_map(|path| {
            let modified = path.metadata().and_then(|m| m.modified()).ok()?;
            Some((modified, path))
        })
        .collect();
    covers.sort_by_key(|cover| std::cmp::Reverse(cover.0));
    for (_, path) in covers.into_iter().skip(keep) {
        let _ = std::fs::remove_file(path);
    }
}

#[derive(Deserialize)]
struct RawClient {
    #[serde(default)]
    address: String,
    #[serde(default)]
    pid: i64,
    /// `[x, y]` in layout coordinates.
    #[serde(default)]
    at: [i32; 2],
    /// `[width, height]`, logical.
    #[serde(default)]
    size: [i32; 2],
    #[serde(default)]
    monitor: i64,
    /// Never given to another window, unlike the address: that is a pointer, and a window
    /// opened just after another closed often lands on the one it left.
    #[serde(default, rename = "stableId")]
    stable_id: String,
}

impl RawClient {
    /// What tells this window apart from every other, the address where Hyprland is too
    /// old to give a stable id.
    fn identity(&self) -> &str {
        if self.stable_id.is_empty() {
            &self.address
        } else {
            &self.stable_id
        }
    }
}

#[derive(Deserialize)]
struct RawMonitor {
    id: i64,
    #[serde(default)]
    scale: f64,
}

/// The terminal's grid: its cells, and the physical pixels the pty says they cover.
#[derive(Debug, Clone, Copy)]
pub struct Grid {
    pub columns: u16,
    pub rows: u16,
    pub width_px: u16,
    pub height_px: u16,
}

/// Where the top-left corner of a cell lands, in Hyprland's layout coordinates.
///
/// The pty gives the grid in physical pixels; Hyprland gives the window's place and
/// size in logical ones. Whatever the window has beyond the grid is taken to be padding
/// split evenly between the two sides, which is what Alacritty's `dynamic_padding` does.
/// With fixed padding instead the error stays under a cell.
pub fn cell_origin(
    at: [i32; 2],
    size: [i32; 2],
    scale: f64,
    grid: Grid,
    column: u16,
    row: u16,
) -> Option<(i32, i32)> {
    if grid.columns == 0
        || grid.rows == 0
        || grid.width_px == 0
        || grid.height_px == 0
        || scale <= 0.0
    {
        return None;
    }
    let axis = |at: i32, size: i32, cells: u16, pixels: u16, cell: u16| {
        let pixels = f64::from(pixels);
        let padding = (f64::from(size) * scale - pixels).max(0.0) / 2.0;
        let offset = padding + pixels / f64::from(cells) * f64::from(cell);
        (f64::from(at) + offset / scale).round() as i32
    };
    Some((
        axis(at[0], size[0], grid.columns, grid.width_px, column),
        axis(at[1], size[1], grid.rows, grid.height_px, row),
    ))
}

/// The image window to move and where it goes: `(address, identity, x, y)`.
///
/// The image window is one of the layer's own (`pid`) other than the one moved last,
/// told apart by `identity`: Überzug++ opens a new window for every image and closes the
/// old one a moment later.
fn window_target(
    clients_json: &str,
    monitors_json: &str,
    own: &str,
    pid: u32,
    moved: Option<&str>,
    grid: Grid,
    placement: &Placement,
) -> Option<(String, String, i32, i32)> {
    let clients: Vec<RawClient> = serde_json::from_str(clients_json).ok()?;
    let image = clients
        .iter()
        .find(|client| client.pid == i64::from(pid) && Some(client.identity()) != moved)?;
    let terminal = clients
        .iter()
        .find(|client| normalise(&client.address) == own)?;
    let scale = own_scale(clients_json, monitors_json, own)?;
    let (x, y) = cell_origin(
        terminal.at,
        terminal.size,
        scale,
        grid,
        placement.x,
        placement.y,
    )?;
    Some((image.address.clone(), image.identity().to_string(), x, y))
}

/// The scale of the monitor pippipit's own window is on.
fn own_scale(clients_json: &str, monitors_json: &str, own: &str) -> Option<f64> {
    let clients: Vec<RawClient> = serde_json::from_str(clients_json).ok()?;
    let monitors: Vec<RawMonitor> = serde_json::from_str(monitors_json).ok()?;
    let terminal = clients
        .iter()
        .find(|client| normalise(&client.address) == own)?;
    monitors
        .iter()
        .find(|monitor| monitor.id == terminal.monitor)
        .map(|monitor| monitor.scale)
        .filter(|scale| *scale > 0.0)
}

/// How many more cells to ask Überzug++ for, so the image fills the ones it was given.
///
/// It draws the image at the pixel size of the cells it is given, then hands it to the
/// compositor at the scale rounded up to a whole number. On a fractional scale the image
/// comes out smaller by that rounding, and asking for proportionally more cells makes
/// up for it. On a whole-number scale this is 1.
fn size_factor(scale: f64) -> f64 {
    if scale > 0.0 {
        scale.ceil() / scale
    } else {
        1.0
    }
}

/// `cells` grown by `factor`, never fewer than it started with.
fn scaled_cells(cells: u16, factor: f64) -> u16 {
    ((f64::from(cells) * factor).round() as u16).max(cells)
}

/// A line sent down Hyprland's socket about the image window, from a template that
/// may name `{address}`, `{x}` and `{y}`.
fn dispatch_command(template: &str, address: &str, x: i32, y: i32) -> String {
    format!(
        "dispatch {}",
        template
            .replace("{address}", address)
            .replace("{x}", &x.to_string())
            .replace("{y}", &y.to_string())
    )
}

/// Hyprland writes addresses with `0x` in its JSON and without it on socket2.
fn normalise(address: &str) -> String {
    address.trim().trim_start_matches("0x").to_string()
}

/// The window pippipit is running in: the client whose pid is the nearest of `ancestors`.
///
/// The nearest, because the terminal is what Hyprland knows as a client, and whatever
/// started the terminal may be a client too.
pub fn own_address(clients_json: &str, ancestors: &[u32]) -> Option<String> {
    let clients: Vec<RawClient> = serde_json::from_str(clients_json).ok()?;
    ancestors.iter().find_map(|pid| {
        clients
            .iter()
            .find(|client| client.pid == i64::from(*pid) && !client.address.is_empty())
            .map(|client| normalise(&client.address))
    })
}

/// The active window from `j/activewindow`, which is `{}` when nothing has focus.
pub fn active_address(json: &str) -> Option<String> {
    let client: RawClient = serde_json::from_str(json).ok()?;
    let address = normalise(&client.address);
    (!address.is_empty()).then_some(address)
}

/// This process and every process above it, nearest first.
fn ancestors() -> Vec<u32> {
    let mut chain = Vec::new();
    let mut pid = std::process::id();
    // A cycle cannot happen, but a bound costs nothing.
    while pid > 1 && chain.len() < 64 {
        chain.push(pid);
        let Some(parent) = parent_of(pid) else {
            break;
        };
        pid = parent;
    }
    chain
}

/// The parent from `/proc/<pid>/stat`. The name in parentheses may hold spaces and
/// parentheses of its own, so the fields are counted from the last `)`.
fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// The line that shows `path` at `placement`, with the frame grown by `factor`.
fn add_command(path: &Path, placement: &Placement, factor: f64) -> String {
    serde_json::json!({
        "action": "add",
        "identifier": IDENTIFIER,
        "x": placement.x,
        "y": placement.y,
        "max_width": scaled_cells(placement.width, factor),
        "max_height": scaled_cells(placement.height, factor),
        "path": path.to_string_lossy(),
        // Covers come in every size. The default only ever shrinks, which leaves a
        // small one sitting in the corner of the frame.
        "scaler": "fit_contain",
    })
    .to_string()
}

fn redispatch_command() -> String {
    serde_json::json!({ "action": "remove", "identifier": IDENTIFIER }).to_string()
}

/// A running `ueberzugpp layer`, fed on stdin.
struct Layer {
    child: Child,
    stdin: ChildStdin,
}

impl Layer {
    fn spawn(program: &str, output: &str) -> Result<Self, String> {
        let mut command = Command::new(program);
        command
            .args(["layer", "--silent", "--output", output])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Its own group, so a stop takes anything it started with it.
            .process_group(0);
        // Stdin closing ends it too, but only once it next polls: this ends it at once,
        // image window and all.
        let mut child = proc::die_with_parent(&mut command)
            .spawn()
            .map_err(|e| format!("spawn {program}: {e}"))?;
        let Some(stdin) = child.stdin.take() else {
            proc::kill_group(&mut child);
            return Err(format!("{program}: stdin unavailable"));
        };
        Ok(Self { child, stdin })
    }

    /// One command. A layer that has gone away fails here rather than raising SIGPIPE:
    /// Rust ignores that signal, so the write comes back as an error.
    fn send(&mut self, line: &str) -> std::io::Result<()> {
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Layer {
    fn drop(&mut self) {
        proc::kill_group(&mut self.child);
    }
}

/// What the coordinator knows, and the layer it runs.
struct Art {
    config: ArtConfig,
    commands: CommandsConfig,
    /// What the panel last asked for.
    want: Option<Placement>,
    /// The active window. `None` until it has been asked for or reported.
    focused: Option<String>,
    /// pippipit's own window. `None` until it has been found.
    own: Option<String>,
    layer: Option<Layer>,
    /// What the layer was last told to show.
    shown: Option<(PathBuf, Placement)>,
    /// The last URL looked up and where it led, so a placement that only moved does not
    /// download the cover again.
    resolved: Option<(String, Option<PathBuf>)>,
    /// The identity of the image window moved last, so the next image's window is not
    /// mistaken for the one it replaces.
    moved: Option<String>,
}

impl Art {
    fn new(config: ArtConfig, commands: CommandsConfig) -> Self {
        Self {
            config,
            commands,
            want: None,
            focused: None,
            own: None,
            layer: None,
            shown: None,
            resolved: None,
            moved: None,
        }
    }

    fn take(&mut self, command: ArtCommand) {
        match command {
            ArtCommand::Show(placement) => self.want = Some(placement),
            ArtCommand::Hide => self.want = None,
            ArtCommand::Focus(address) => self.focused = Some(normalise(&address)),
        }
    }

    /// Bring the layer in line with what was asked for.
    fn reconcile(&mut self, runner: &dyn CommandRunner, stop: &Shutdown) {
        if self.layer.as_mut().is_some_and(|layer| !layer.alive()) {
            self.layer = None;
            self.shown = None;
        }
        if self.layer.is_none()
            && self.may_start()
            && let Ok(layer) = Layer::spawn(&self.commands.ueberzugpp, &self.config.output)
        {
            self.layer = Some(layer);
        }

        let target = match self.want.clone() {
            Some(placement) => self
                .path_for(&placement.url, runner)
                .map(|path| (path, placement)),
            None => None,
        };
        if self.layer.is_none() {
            return;
        }
        let line = match (&target, &self.shown) {
            (Some(target), shown) if shown.as_ref() != Some(target) => {
                add_command(&target.0, &target.1, self.size_factor())
            }
            (None, Some(_)) => redispatch_command(),
            _ => return,
        };
        let Some(layer) = self.layer.as_mut() else {
            return;
        };
        if layer.send(&line).is_err() {
            self.layer = None;
            self.shown = None;
            return;
        }
        self.shown = target;
        if self.config.output == "wayland"
            && let Some((_, placement)) = self.shown.clone()
        {
            self.place_window(&placement, stop);
        }
    }

    /// The factor to grow the frame by, from the scale of the monitor the panel is on.
    ///
    /// Only the Wayland output hands the image to the compositor at a rounded scale, and
    /// it is asked for here rather than kept, since the pane can move to another monitor.
    fn size_factor(&self) -> f64 {
        if self.config.output != "wayland" {
            return 1.0;
        }
        let Some(own) = self.own.as_deref() else {
            return 1.0;
        };
        let timeout = self.commands.hyprland_timeout();
        hyprland::request("j/clients", timeout)
            .ok()
            .zip(hyprland::request("j/monitors", timeout).ok())
            .and_then(|(clients, monitors)| own_scale(&clients, &monitors, own))
            .map_or(1.0, size_factor)
    }

    /// Put the image window over its frame, once Überzug++ has opened it.
    ///
    /// The window turns up a moment after the add, so this waits for it, and gives up
    /// quietly if it never does: the image is still shown, only not where it belongs.
    fn place_window(&mut self, placement: &Placement, stop: &Shutdown) {
        let (Some(layer), Some(own)) = (&self.layer, self.own.clone()) else {
            return;
        };
        let pid = layer.child.id();
        // The pty's size, pixels included. pippipit's stdout is the terminal.
        let Ok(size) = crossterm::terminal::window_size() else {
            return;
        };
        let grid = Grid {
            columns: size.columns,
            rows: size.rows,
            width_px: size.width,
            height_px: size.height,
        };
        let timeout = self.commands.hyprland_timeout();
        let deadline = Instant::now() + WINDOW_WAIT;
        loop {
            let target = hyprland::request("j/clients", timeout)
                .ok()
                .zip(hyprland::request("j/monitors", timeout).ok())
                .and_then(|(clients, monitors)| {
                    window_target(
                        &clients,
                        &monitors,
                        &own,
                        pid,
                        self.moved.as_deref(),
                        grid,
                        placement,
                    )
                });
            if let Some((address, identity, x, y)) = target {
                let command = dispatch_command(&self.config.move_window, &address, x, y);
                let _ = hyprland::request(&command, timeout);
                // Only after the move, so a window opened transparent is never seen
                // anywhere but over its frame.
                if !self.config.show.is_empty() {
                    let command = dispatch_command(&self.config.show, &address, x, y);
                    let _ = hyprland::request(&command, timeout);
                }
                self.moved = Some(identity);
                return;
            }
            if Instant::now() >= deadline || !stop.sleep(WINDOW_POLL) {
                return;
            }
        }
    }

    /// Whether a layer started now would land on the right window.
    ///
    /// Only the Wayland output has to ask. The others draw through the terminal itself.
    fn may_start(&mut self) -> bool {
        if self.config.output != "wayland" {
            return true;
        }
        let timeout = self.commands.hyprland_timeout();
        if self.own.is_none() {
            self.own = hyprland::request("j/clients", timeout)
                .ok()
                .and_then(|json| own_address(&json, &ancestors()));
        }
        if self.focused.is_none() {
            self.focused = hyprland::request("j/activewindow", timeout)
                .ok()
                .and_then(|json| active_address(&json));
        }
        self.own.is_some() && self.own == self.focused
    }

    /// The local file behind a URL: downloaded if it is remote, and taken out of the song
    /// cmus is playing for cmus's stand-in.
    ///
    /// A local file is looked at every time rather than remembered: the player may have
    /// deleted it since, and a path that is gone shows nothing but the placeholder.
    fn path_for(&mut self, url: &str, runner: &dyn CommandRunner) -> Option<PathBuf> {
        let found = source(url);
        if let Some(Source::File(path)) = found {
            return path.is_file().then_some(path);
        }
        if let Some((looked_up, path)) = &self.resolved
            && looked_up == url
        {
            return path.clone();
        }
        let path = match found {
            Some(Source::Remote(url)) => {
                cache_dir().and_then(|dir| fetch(&url, &dir, &self.commands.curl, runner).ok())
            }
            Some(Source::Cmus) => runner
                .output(&self.commands.cmus_remote, &["-Q"])
                .ok()
                .as_deref()
                .and_then(cmus_file)
                .zip(cache_dir())
                .and_then(|(song, dir)| local_cover(&song, &dir, &self.commands.ffmpeg, runner)),
            Some(Source::File(_)) | None => None,
        };
        self.resolved = Some((url.to_string(), path.clone()));
        path
    }
}

/// Runs the layer, and downloads covers for it, on a thread of its own.
///
/// It reports nothing back: there is no reading to show, and a cover that cannot be
/// shown leaves the frame's placeholder standing.
pub struct ArtProvider {
    pub config: ArtConfig,
    pub commands: CommandsConfig,
}

impl ArtProvider {
    pub fn spawn(self) -> Result<ProviderHandle<ArtCommand>, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let shutdown = Shutdown::new();
        let stop = shutdown.clone();
        let join = std::thread::Builder::new()
            .name("pippipit-art".into())
            .spawn(move || {
                let runner = SystemRunner::new(self.config.download_timeout(), stop.clone());
                let mut art = Art::new(self.config, self.commands);
                // The panel is usually the active window right after it starts, which is
                // the one moment a layer can be started without waiting to be focused.
                art.reconcile(&runner, &stop);
                while let Some(commands) =
                    wait_for_commands(&rx, &stop, Instant::now() + IDLE_WAIT, COALESCE_WINDOW)
                {
                    for command in commands {
                        art.take(command);
                    }
                    art.reconcile(&runner, &stop);
                }
                // `art` goes out of scope here, and the layer with it.
            })
            .map_err(|e| format!("failed to spawn the art thread: {e}"))?;
        Ok(ProviderHandle::new(tx, shutdown, join))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement(url: &str, x: u16) -> Placement {
        Placement {
            url: url.into(),
            x,
            y: 9,
            width: 10,
            height: 5,
            terminal: (145, 18),
        }
    }

    #[test]
    fn a_file_url_is_decoded_into_a_path() {
        assert_eq!(
            source("file:///tmp/pippipit%20test/cover.png"),
            Some(Source::File(PathBuf::from("/tmp/pippipit test/cover.png")))
        );
    }

    #[test]
    fn a_remote_url_is_kept_for_curl() {
        assert_eq!(
            source("https://example.com/cover.jpg"),
            Some(Source::Remote("https://example.com/cover.jpg".into()))
        );
    }

    fn track(player: &str, title: &str) -> Media {
        Media {
            player: player.into(),
            status: crate::sources::media::Status::Playing,
            position_us: 0,
            length_us: 267_000_000,
            art_url: String::new(),
            title: title.into(),
            artist: "Sample Artist".into(),
            album: "Sample Album".into(),
        }
    }

    /// cmus names no art, so its track stands in for the URL, and a new track is a new
    /// lookup. Any other player with no art has nothing to look up.
    #[test]
    fn cmus_stands_in_for_its_missing_art_url() {
        let one = url_for(&track("cmus", "One")).expect("cmus is asked for its file");
        assert_eq!(source(&one), Some(Source::Cmus));
        assert_ne!(Some(one), url_for(&track("cmus", "Two")));

        assert_eq!(url_for(&track("firefox", "One")), None);
        let given = Media {
            art_url: "https://example.com/a.jpg".into(),
            ..track("cmus", "One")
        };
        assert_eq!(
            url_for(&given).as_deref(),
            Some("https://example.com/a.jpg"),
            "a player's own art URL always wins"
        );
    }

    #[test]
    fn the_playing_file_comes_from_cmus_remote() {
        let status =
            "status playing\nfile /music/Sample Artist/Sample Album/01 One.flac\nduration 267\n";
        assert_eq!(
            cmus_file(status),
            Some(PathBuf::from(
                "/music/Sample Artist/Sample Album/01 One.flac"
            ))
        );
        assert_eq!(cmus_file("status stopped\n"), None);
    }

    /// A fresh directory of its own for a test, removed first in case a run was cut short.
    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("pippipit-test-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("album")).unwrap();
        root
    }

    /// With nothing embedded, the cover is the image beside the song.
    #[test]
    fn a_song_without_an_embedded_cover_uses_the_one_beside_it() {
        let root = scratch("side-cover");
        let song = root.join("album").join("01.flac");
        std::fs::write(&song, b"not really flac").unwrap();
        let runner = SystemRunner::new(Duration::from_secs(1), Shutdown::new());
        let cache = root.join("cache");

        assert_eq!(local_cover(&song, &cache, "false", &runner), None);
        std::fs::write(root.join("album").join("folder.jpg"), b"jpg").unwrap();
        assert_eq!(
            local_cover(&song, &cache, "false", &runner),
            Some(root.join("album").join("folder.jpg"))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An embedded cover is taken out once, and found in the cache after that.
    #[test]
    fn an_embedded_cover_is_taken_out_once() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("embedded-cover");
        let song = root.join("album").join("01.flac");
        std::fs::write(&song, b"not really flac").unwrap();
        let calls = root.join("calls");
        // Stands in for ffmpeg: writes an image to its last argument and counts the calls.
        let ffmpeg = root.join("ffmpeg");
        std::fs::write(
            &ffmpeg,
            format!(
                "#!/bin/sh\nfor last; do :; done\nprintf png > \"$last\"\necho call >> '{}'\n",
                calls.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();
        let runner = SystemRunner::new(Duration::from_secs(1), Shutdown::new());
        let cache = root.join("cache");
        let program = ffmpeg.to_string_lossy();

        let first = local_cover(&song, &cache, &program, &runner).expect("taken out");
        assert_eq!(std::fs::read(&first).unwrap(), b"png");
        assert_eq!(local_cover(&song, &cache, &program, &runner), Some(first));
        assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Nothing to fetch and nothing to open: no art rather than an error.
    #[test]
    fn anything_else_has_no_source() {
        for url in [
            "",
            "data:image/png;base64,AAAA",
            "file://relative/cover.png",
            "file:///bad%zzescape",
        ] {
            assert_eq!(source(url), None, "{url}");
        }
    }

    /// The cache name has to survive a restart, or every run downloads everything again.
    #[test]
    fn the_cache_name_is_fixed_for_a_url() {
        assert_eq!(cache_name(""), "cbf29ce484222325");
        assert_eq!(
            cache_name("https://example.com/a"),
            cache_name("https://example.com/a")
        );
        assert_ne!(
            cache_name("https://example.com/a"),
            cache_name("https://example.com/b")
        );
    }

    /// The terminal is the client; the compositor that started it may be one too.
    #[test]
    fn the_own_window_is_the_nearest_ancestor_hyprland_knows() {
        let clients = r#"[
            {"address":"0xaaa","pid":100},
            {"address":"0xbbb","pid":300},
            {"address":"0xccc","pid":200}
        ]"#;
        assert_eq!(
            own_address(clients, &[400, 300, 200, 100]),
            Some("bbb".into())
        );
        assert_eq!(own_address(clients, &[400, 500]), None);
        assert_eq!(own_address("not json", &[300]), None);
    }

    #[test]
    fn the_active_window_matches_socket2s_spelling() {
        assert_eq!(
            active_address(r#"{"address":"0x55d3b1c0a2f0","pid":1}"#),
            Some("55d3b1c0a2f0".into())
        );
        assert_eq!(active_address("{}"), None);
    }

    #[test]
    fn this_process_is_first_among_its_ancestors() {
        let chain = ancestors();
        assert_eq!(chain.first(), Some(&std::process::id()));
        assert!(chain.len() > 1, "a test runs under something");
    }

    #[test]
    fn the_add_command_says_where_and_what() {
        let line = add_command(Path::new("/tmp/cover.png"), &placement("u", 2), 1.0);
        let json: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(json["action"], "add");
        assert_eq!(json["identifier"], IDENTIFIER);
        assert_eq!(json["x"], 2);
        assert_eq!(json["y"], 9);
        assert_eq!(json["max_width"], 10);
        assert_eq!(json["max_height"], 5);
        assert_eq!(json["path"], "/tmp/cover.png");
        assert!(!line.contains('\n'), "one command is one line");
    }

    /// A 143x17 grid of 13x30-pixel cells in a 1140x318 window at a scale of 5/3, with
    /// Alacritty's even padding: the cell at column 2, row 10 starts 28 and 186 logical
    /// pixels in.
    #[test]
    fn a_cell_lands_past_the_padding_at_the_monitors_scale() {
        let grid = Grid {
            columns: 143,
            rows: 17,
            width_px: 1859,
            height_px: 510,
        };
        assert_eq!(
            cell_origin([1156, 1448], [1140, 318], 5.0 / 3.0, grid, 2, 10),
            Some((1184, 1634))
        );
        assert_eq!(
            cell_origin([0, 0], [100, 100], 1.0, grid, 0, 0),
            Some((0, 0)),
            "a window smaller than its grid has no padding to skip"
        );
        let empty = Grid { columns: 0, ..grid };
        assert_eq!(cell_origin([0, 0], [100, 100], 1.0, empty, 0, 0), None);
    }

    /// The image window is the layer's, and not the one it is about to replace.
    #[test]
    fn the_image_window_is_the_layers_newest() {
        let clients = r#"[
            {"address":"0xaaa","pid":10,"at":[1156,1448],"size":[1140,318],"monitor":2},
            {"address":"0xold","pid":42,"at":[1120,1738],"size":[65,65],"monitor":2},
            {"address":"0xnew","pid":42,"at":[1120,1738],"size":[65,65],"monitor":2}
        ]"#;
        let monitors = r#"[{"id":0,"scale":1.0},{"id":2,"scale":1.6666666}]"#;
        let grid = Grid {
            columns: 143,
            rows: 17,
            width_px: 1859,
            height_px: 510,
        };
        let placement = Placement {
            y: 10,
            ..placement("u", 2)
        };
        assert_eq!(
            window_target(
                clients,
                monitors,
                "aaa",
                42,
                Some("0xold"),
                grid,
                &placement
            ),
            Some(("0xnew".into(), "0xnew".into(), 1184, 1634))
        );
        assert_eq!(
            window_target(clients, monitors, "aaa", 43, None, grid, &placement),
            None,
            "no window of the layer's yet"
        );
    }

    /// An image removed and then added again gets a window at the address the old one
    /// left. The stable id still tells them apart.
    #[test]
    fn a_reused_address_is_still_a_new_window() {
        let clients = r#"[
            {"address":"0xaaa","pid":10,"at":[1156,1448],"size":[1140,318],"monitor":2,"stableId":"18000004"},
            {"address":"0xbbb","pid":42,"at":[1120,1738],"size":[65,65],"monitor":2,"stableId":"1800004c"}
        ]"#;
        let monitors = r#"[{"id":2,"scale":1.0}]"#;
        let grid = Grid {
            columns: 100,
            rows: 10,
            width_px: 1000,
            height_px: 300,
        };
        let placement = placement("u", 0);
        let target = |moved| window_target(clients, monitors, "aaa", 42, moved, grid, &placement);
        assert_eq!(
            target(Some("1800004b")).map(|(address, identity, ..)| (address, identity)),
            Some(("0xbbb".into(), "1800004c".into())),
            "same address as the window moved last, but a new stable id"
        );
        assert_eq!(target(Some("1800004c")), None, "the window moved last");
    }

    /// At a scale of 5/3 the image is handed over at 2 and comes out 5/6 the size, so a
    /// 7x3 frame is asked for as 8x4. A whole-number scale asks for the frame as it is.
    #[test]
    fn a_fractional_scale_asks_for_a_bigger_frame() {
        let factor = size_factor(5.0 / 3.0);
        assert_eq!((scaled_cells(7, factor), scaled_cells(3, factor)), (8, 4));
        for scale in [1.0, 2.0] {
            let factor = size_factor(scale);
            assert_eq!((scaled_cells(7, factor), scaled_cells(3, factor)), (7, 3));
        }
        assert_eq!(size_factor(0.0), 1.0);

        let line = add_command(Path::new("/tmp/cover.png"), &placement("u", 2), factor);
        let json: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            (json["max_width"].clone(), json["max_height"].clone()),
            (12.into(), 6.into()),
            "a 10x5 frame at the same scale"
        );
    }

    #[test]
    fn the_scale_is_the_one_under_the_panels_window() {
        let clients = r#"[
            {"address":"0xaaa","pid":10,"monitor":2},
            {"address":"0xbbb","pid":11,"monitor":0}
        ]"#;
        let monitors = r#"[{"id":0,"scale":1.0},{"id":2,"scale":1.6666666}]"#;
        assert_eq!(own_scale(clients, monitors, "aaa"), Some(1.6666666));
        assert_eq!(own_scale(clients, monitors, "bbb"), Some(1.0));
        assert_eq!(own_scale(clients, monitors, "ccc"), None);
    }

    /// The show template needs only the address.
    #[test]
    fn the_show_command_names_the_window() {
        assert_eq!(
            dispatch_command(
                "hl.dsp.window.set_prop({ prop = 'opacity_inactive', value = '1', window = 'address:{address}' })",
                "0xabc",
                1184,
                1634
            ),
            "dispatch hl.dsp.window.set_prop({ prop = 'opacity_inactive', value = '1', window = 'address:0xabc' })"
        );
        assert!(
            ArtConfig::default().show.is_empty(),
            "nothing is sent by default"
        );
    }

    /// The template decides the syntax, so an override can have its own.
    #[test]
    fn the_move_command_fills_in_the_template() {
        assert_eq!(
            dispatch_command(&ArtConfig::default().move_window, "0xabc", 1184, 1634),
            "dispatch movewindowpixel exact 1184 1634,address:0xabc"
        );
        assert_eq!(
            dispatch_command(
                "hl.dsp.window.move({ x = {x}, y = {y}, window = 'address:{address}' })",
                "0xabc",
                1,
                2
            ),
            "dispatch hl.dsp.window.move({ x = 1, y = 2, window = 'address:0xabc' })"
        );
    }

    /// Only the newest placement and the newest focus matter, and neither may swallow
    /// the other.
    #[test]
    fn a_burst_keeps_the_newest_placement_and_the_newest_focus() {
        let burst = vec![
            ArtCommand::Show(placement("a", 1)),
            ArtCommand::Focus("1".into()),
            ArtCommand::Hide,
            ArtCommand::Focus("2".into()),
            ArtCommand::Show(placement("b", 2)),
        ];
        assert_eq!(
            crate::sources::provider::coalesce(burst),
            vec![
                ArtCommand::Show(placement("b", 2)),
                ArtCommand::Focus("2".into())
            ]
        );
    }

    /// A stand-in for Überzug++ that writes what it is told to a file beside itself.
    fn fake_layer(name: &str) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let script = std::env::temp_dir().join(format!("pippipit-test-layer-{name}"));
        let out = script.with_extension("out");
        let _ = std::fs::remove_file(&out);
        std::fs::write(
            &script,
            format!("#!/bin/sh\nexec cat > '{}'\n", out.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (script, out)
    }

    fn lines_once_settled(out: &Path, want: usize) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let lines: Vec<String> = std::fs::read_to_string(out)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect();
            if lines.len() >= want || Instant::now() > deadline {
                return lines;
            }
            std::thread::yield_now();
        }
    }

    /// The layer hears about a change once: a placement that did not move is not sent
    /// again, and taking the art away sends the remove.
    #[test]
    fn the_layer_is_told_only_what_changed() {
        let (script, out) = fake_layer("changes");
        let mut art = Art::new(
            ArtConfig {
                output: "kitty".into(),
                ..ArtConfig::default()
            },
            CommandsConfig {
                ueberzugpp: script.to_string_lossy().into_owned(),
                ..CommandsConfig::default()
            },
        );
        let runner = SystemRunner::new(Duration::from_secs(1), Shutdown::new());
        let root = scratch("changes");
        let cover = root.join("cover.png");
        std::fs::write(&cover, b"png").unwrap();
        let url = format!("file://{}", cover.display());

        art.take(ArtCommand::Show(placement(&url, 2)));
        art.reconcile(&runner, &Shutdown::new());
        art.take(ArtCommand::Show(placement(&url, 2)));
        art.reconcile(&runner, &Shutdown::new());
        art.take(ArtCommand::Show(placement(&url, 4)));
        art.reconcile(&runner, &Shutdown::new());
        art.take(ArtCommand::Hide);
        art.reconcile(&runner, &Shutdown::new());
        art.take(ArtCommand::Hide);
        art.reconcile(&runner, &Shutdown::new());

        let lines = lines_once_settled(&out, 3);
        drop(art);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].contains(r#""x":2"#), "{}", lines[0]);
        assert!(lines[1].contains(r#""x":4"#), "{}", lines[1]);
        assert!(lines[2].contains(r#""remove""#), "{}", lines[2]);
    }

    /// A local cover is looked for every time: one the player has deleted since is gone,
    /// even under a URL that led to it before.
    #[test]
    fn a_deleted_local_cover_is_not_shown() {
        let root = scratch("deleted-cover");
        let cover = root.join("cover.png");
        std::fs::write(&cover, b"png").unwrap();
        let url = format!("file://{}", cover.display());
        let mut art = Art::new(ArtConfig::default(), CommandsConfig::default());
        let runner = SystemRunner::new(Duration::from_secs(1), Shutdown::new());

        assert_eq!(art.path_for(&url, &runner), Some(cover.clone()));
        std::fs::remove_file(&cover).unwrap();
        assert_eq!(art.path_for(&url, &runner), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Without an address that is its own, the Wayland output never starts a layer: it
    /// would put the art over some other window.
    #[test]
    fn the_wayland_layer_waits_for_the_panels_own_window() {
        let (script, _) = fake_layer("waits");
        let mut art = Art::new(
            ArtConfig::default(),
            CommandsConfig {
                ueberzugpp: script.to_string_lossy().into_owned(),
                ..CommandsConfig::default()
            },
        );
        art.own = Some("aaa".into());
        art.take(ArtCommand::Focus("0xbbb".into()));
        let runner = SystemRunner::new(Duration::from_secs(1), Shutdown::new());
        art.reconcile(&runner, &Shutdown::new());
        assert!(art.layer.is_none(), "another window has focus");

        art.take(ArtCommand::Focus("aaa".into()));
        art.reconcile(&runner, &Shutdown::new());
        assert!(art.layer.is_some(), "the panel's own window has focus");
    }
}
