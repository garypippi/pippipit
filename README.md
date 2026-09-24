# pippipit

A desktop TUI dashboard built for me.

## Usage

```
pippipit [OPTIONS]
  -c, --config <PATH>   config file (default: $XDG_CONFIG_HOME/pippipit/config.toml)
      --no-mouse        do not capture the mouse
  -h, --help
  -V, --version
```

| key | |
|---|---|
| `r` | refresh everything |
| `p` | hide the SSID and the address, for a screenshot |
| `?` | help |
| `q` | quit |

Everything else is the mouse: click or scroll a volume bar, click the mute icon,
the transport buttons, the seek bar, a workspace or a power button.

| signal | |
|---|---|
| `SIGUSR1` | refresh volume |
| `SIGUSR2` | refresh everything |

```sh
pkill -USR1 -x pippipit
```

## Configuration

The config file is optional. An unknown key is an error.
[`config.example.toml`](config.example.toml) lists every setting with its default.

```toml
[general]
layout = "auto"   # auto | 2-pane | 2-pane-narrow | 1-pane | bar

[workspaces]
count = 8

[audio]
step       = 1
max_volume = 150

[power]
suspend  = ["loginctl", "suspend"]
poweroff = ["loginctl", "poweroff"]

[theme]
accent    = "cyan"
nerd_font = true
frame     = "full"   # full | top

[theme.clock]         # also workspaces, audio, media, sensors, network, power
accent = "#ebcb8b"
```
