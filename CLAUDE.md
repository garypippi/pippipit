# pippipit

A desktop status TUI for Hyprland, in Rust + ratatui.

## Build and check

```sh
cargo test
cargo clippy --all-targets   # must stay clean
cargo fmt
```

## Architecture

- **Provider** (`src/sources/`) owns one thread and is the only place anything
  blocks. Every blocking wait returns within `STOP_CHECK_INTERVAL` to check the
  stop flag: use `Shutdown::sleep`, never `thread::sleep`.
- **Store** (`src/store.rs`) is pure and synchronous. A provider restarting must
  not lose history.
- Only the event loop's thread touches state, through `AppState::reduce`.
- Widgets never handle input. Clicks resolve against the `HitMap`
  (`src/ui/hit.rs`) that drawing produced.

## Constraints

- hwmon indices change between boots. Never store a `/sys/class/hwmon/hwmonN`
  path; sensor keys are `<hwmon name>/<temp label>`.
- calloop cannot take realtime signals, so only `SIGUSR1` (volume) and `SIGUSR2`
  (everything) exist.
- Every Nerd Font glyph needs an ASCII fallback (`nerd_font = false`).
- Shipped defaults use upstream Hyprland syntax.

## House rules

- Comments are self-contained: what the code does and what would break. No
  history, no pointers to other documents.
- Dependencies stay few. No tokio, no zbus, no `pipewire-rs`. Hyprland is
  reached through its sockets; `hyprctl` appears only as a configurable power
  command.
- An unknown config key is an error.
- Never commit an image.
- The repository is public. No MAC, BSSID, SSID, user name, host name, serial
  or track title in fixtures or docs; use made-up values like `MyNetwork-5G`.
- Stop a running panel with `pkill -x pippipit`, never `pkill -f` (it matches
  the calling shell too).
- Commit messages are one line, present tense, saying what the change is for.
- pippipit owns the keys `r`, `?` and `q`; everything else is the mouse.
- `README.md` covers usage and configuration, `config.example.toml` lists every
  setting with its default, and both are kept true. There are no design
  documents.
