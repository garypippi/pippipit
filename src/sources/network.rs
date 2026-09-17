//! Networking, for systems without NetworkManager (netifrc + wpa_supplicant + dhcpcd).
//!
//! - The interface list and kind come from `/sys/class/net`
//! - Addresses come from `ip -j addr` (JSON)
//! - Wireless details come from `wpa_cli status` and `signal_poll`
//! - Throughput is the delta of `/proc/net/dev`
//!
//! **`/proc/net/wireless` only exists on kernels built with `CONFIG_CFG80211_WEXT`**,
//! so signal strength comes from `signal_poll`, which works either way.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::config::{CommandsConfig, NetworkConfig};
use crate::sources::provider::{
    COALESCE_WINDOW, Coalesce, CommandRunner, Fold, ProviderHandle, Shutdown, SystemRunner,
    wait_for_commands,
};

#[derive(Debug, Clone, PartialEq)]
pub struct Wireless {
    pub ssid: Option<String>,
    pub freq_mhz: Option<u32>,
    pub key_mgmt: Option<String>,
    /// `pairwise_cipher`. Needed because `key_mgmt` alone cannot tell an open network
    /// from a WEP one - both report `NONE`, and only the cipher says `WEP-40` /
    /// `WEP-104`. It comes out of the same `status` call, so this costs no extra work.
    pub cipher: Option<String>,
    /// dBm, from `signal_poll`'s `RSSI`.
    pub rssi: Option<i32>,
    /// Mb/s, from `signal_poll`'s `LINKSPEED`.
    pub link_mbps: Option<u32>,
    /// `wpa_state`. Anything but `COMPLETED` means connecting or disconnected.
    pub state: Option<String>,
}

/// What a link's security amounts to, as far as it is worth telling anyone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Security {
    /// WPA2, WPA3 / SAE, OWE. **Not worth a column.** It is the configuration of the
    /// network rather than its state, and it cannot change while you are attached.
    Current,
    /// No encryption at all.
    Open,
    /// WEP. `key_mgmt` says `NONE` for this as well, which is why the cipher is read:
    /// without it an open network and a WEP one are indistinguishable.
    Wep,
    /// Anything not recognised, carried as it came. Never silently dropped - the same
    /// rule the sensor labels follow: a name that cannot be placed is better raw than
    /// guessed at, and quietly hiding an unknown suite is how you fail to notice one
    /// that should have worried you.
    Unknown(String),
}

/// Whether one suite name is one of the current ones.
///
/// Deliberately short. `WPA-PSK` is WPA1 and does not match; neither does `IEEE8021X`
/// on its own, which is dynamic WEP. Both come out as `Unknown` and get shown, which
/// is the right way round for something the reader may want to act on.
fn suite_is_current(suite: &str) -> bool {
    ["WPA2", "WPA3", "SAE", "OWE", "RSN"]
        .iter()
        .any(|known| suite.contains(known))
}

impl Wireless {
    /// Classify the link's security, or `None` when there is no link to classify.
    pub fn security(&self) -> Option<Security> {
        let raw = self.key_mgmt.as_deref()?;
        let upper = raw.to_uppercase();
        if upper == "NONE" {
            let wep = self
                .cipher
                .as_deref()
                .is_some_and(|c| c.to_uppercase().contains("WEP"));
            return Some(if wep { Security::Wep } else { Security::Open });
        }
        // wpa_supplicant joins several suites with `+`; every one of them has to be
        // current before the whole link is.
        let current = upper
            .split(['+', ' '])
            .filter(|s| !s.is_empty())
            .all(suite_is_current);
        Some(if current {
            Security::Current
        } else {
            Security::Unknown(raw.to_string())
        })
    }
}

/// What sort of interface this is.
///
/// One classifier for two jobs: it decides the sort order (`rank`) and, since the
/// pane draws an icon for it, what that icon is. A second classification living in the
/// UI would be a second thing to keep in step.
///
/// The order of the variants **is** the sort order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Has `/sys/class/net/<if>/wireless`.
    Wireless,
    /// Anything else the machine really has.
    Wired,
    /// Matched `[network] virtual`: bridges, docker, veth, loopback.
    Virtual,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Interface {
    pub name: String,
    pub kind: Kind,
    /// The raw `/sys/class/net/<if>/operstate` value (`up`, `dormant`, `down`, …).
    pub operstate: String,
    /// In the form `192.168.1.10/24`.
    pub ipv4: Option<String>,
    pub wireless: Option<Wireless>,
    /// The cumulative `/proc/net/dev` counters; the rate is derived from the delta in state.
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Network {
    pub interfaces: Vec<Interface>,
}

// ---- /proc/net/dev ----

/// `IF -> (rx_bytes, tx_bytes)`.
pub fn parse_proc_net_dev(text: &str) -> BTreeMap<String, (u64, u64)> {
    let mut out = BTreeMap::new();
    for line in text.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // Receive bytes is the first column; transmit bytes is the ninth.
        if fields.len() < 9 {
            continue;
        }
        let (Ok(rx), Ok(tx)) = (fields[0].parse::<u64>(), fields[8].parse::<u64>()) else {
            continue;
        };
        out.insert(name.trim().to_string(), (rx, tx));
    }
    out
}

// ---- wpa_cli ----

/// Parse the one-`key=value`-per-line format.
pub fn parse_wpa_kv(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

/// wpa_supplicant escapes a non-ASCII SSID as `\xNN`. Put it back into readable form.
pub fn unescape_ssid(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() && bytes[i + 1] == b'x' {
            let hex = &raw[i + 2..i + 4];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn build_wireless(status: &str, signal: &str) -> Wireless {
    let s = parse_wpa_kv(status);
    let p = parse_wpa_kv(signal);
    Wireless {
        ssid: s.get("ssid").map(|v| unescape_ssid(v)),
        freq_mhz: s.get("freq").and_then(|v| v.parse().ok()),
        key_mgmt: s.get("key_mgmt").cloned(),
        cipher: s.get("pairwise_cipher").cloned(),
        rssi: p.get("RSSI").and_then(|v| v.parse().ok()),
        link_mbps: p.get("LINKSPEED").and_then(|v| v.parse().ok()),
        state: s.get("wpa_state").cloned(),
    }
}

// ---- ip -j addr ----

#[derive(Debug, Deserialize)]
struct IpLink {
    ifname: String,
    #[serde(default)]
    addr_info: Vec<IpAddrInfo>,
}

#[derive(Debug, Deserialize)]
struct IpAddrInfo {
    family: String,
    local: String,
    #[serde(default)]
    prefixlen: u8,
}

/// `interface -> "a.b.c.d/nn"`. IPv4 only; there is no room across for IPv6.
pub fn parse_ip_addr(json: &str) -> Result<BTreeMap<String, String>, String> {
    let links: Vec<IpLink> = serde_json::from_str(json).map_err(|e| format!("ip addr: {e}"))?;
    Ok(links
        .into_iter()
        .filter_map(|link| {
            link.addr_info
                .iter()
                .find(|a| a.family == "inet")
                .map(|a| (link.ifname.clone(), format!("{}/{}", a.local, a.prefixlen)))
        })
        .collect())
}

// ---- Assembly ----

fn is_wireless(name: &str) -> bool {
    std::path::Path::new(&format!("/sys/class/net/{name}/wireless")).is_dir()
}

fn read_operstate(name: &str) -> String {
    std::fs::read_to_string(format!("/sys/class/net/{name}/operstate"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Whether a name matches a pattern. A trailing `*` matches by prefix.
fn matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => pattern == name,
    }
}

/// Whether this interface should be shown.
///
/// By default `lo` is excluded, as are the states known to be unusable
/// (`down`, `lowerlayerdown`, `notpresent`).
/// That is what removes `dummy0` and `sit0`.
///
/// **`dormant` stays.** It is the state during wpa authentication right after boot,
/// and hiding it would make the network rows disappear entirely -
/// worse than useless for a status display, since you cannot tell connecting from broken.
///
/// An empty operstate (it could not be read) is always hidden.
pub fn should_show(config: &NetworkConfig, name: &str, operstate: &str) -> bool {
    if !config.interface.is_empty() {
        return config.interface.iter().any(|p| matches(p, name));
    }
    if config.exclude.iter().any(|p| matches(p, name)) {
        return false;
    }
    !operstate.is_empty() && !config.hide_operstate.iter().any(|s| s == operstate)
}

/// Whether this interface is one of the ones that yields the top of the pane.
pub fn is_virtual(config: &NetworkConfig, name: &str) -> bool {
    config.r#virtual.iter().any(|p| matches(p, name))
}

/// Where an interface sorts. Wireless first, then the rest of the hardware, then the
/// virtual ones.
///
/// The pane is four rows and a wireless interface alone is three of them, so this
/// decides what is **seen** rather than merely what is first: with `docker0` and a
/// pair of `veth`s around, alphabetical order pushed the WiFi off the bottom entirely.
fn rank(config: &NetworkConfig, name: &str) -> u8 {
    kind(config, name) as u8
}

/// Classify an interface by name. `virtual` wins over `wireless`: the config naming a
/// thing virtual is a statement about it, and a wireless bridge is still a bridge.
pub fn kind(config: &NetworkConfig, name: &str) -> Kind {
    if is_virtual(config, name) {
        Kind::Virtual
    } else if is_wireless(name) {
        Kind::Wireless
    } else {
        Kind::Wired
    }
}

fn wanted_interfaces(config: &NetworkConfig) -> Result<Vec<(String, String)>, String> {
    let mut found: Vec<(String, String)> = std::fs::read_dir("/sys/class/net")
        .map_err(|e| format!("/sys/class/net: {e}"))?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .map(|name| {
            let state = read_operstate(&name);
            (name, state)
        })
        .filter(|(name, state)| should_show(config, name, state))
        .collect();
    order(config, &mut found);
    Ok(found)
}

/// Put the list in display order.
///
/// `read_dir` has no order of its own, so this is also what makes the pane stable
/// between reads.
pub fn order(config: &NetworkConfig, found: &mut [(String, String)]) {
    found.sort_by(|(a, _), (b, _)| rank(config, a).cmp(&rank(config, b)).then_with(|| a.cmp(b)));
    if !config.interface.is_empty() {
        // An explicit list is an ordering as well as a filter: somebody who writes it
        // out has said which one matters most, and alphabetising it throws that away.
        // The sort is stable, so a pattern matching several keeps them in rank order.
        found.sort_by_key(|(name, _)| {
            config
                .interface
                .iter()
                .position(|p| matches(p, name))
                .unwrap_or(usize::MAX)
        });
    }
}

/// One pass over the interfaces.
///
/// The external commands run side by side: `ip -j addr` alongside the `wpa_cli` pair for
/// each wireless interface. Run one after another they add up to the timeout times three.
pub fn read(
    config: &NetworkConfig,
    commands: &CommandsConfig,
    runner: &dyn CommandRunner,
) -> Result<Network, String> {
    let names = wanted_interfaces(config)?;
    if names.is_empty() {
        return Ok(Network::default());
    }

    let counters = std::fs::read_to_string("/proc/net/dev")
        .map(|t| parse_proc_net_dev(&t))
        .unwrap_or_default();

    let wireless_names: Vec<&str> = names
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| is_wireless(name))
        .collect();

    let (addrs, wireless) = std::thread::scope(|scope| {
        // One `ip -j addr` returns every interface. Never call it per interface.
        let addrs = scope.spawn(|| {
            runner
                .output(&commands.ip, &["-j", "addr"])
                .and_then(|json| parse_ip_addr(&json))
                .unwrap_or_default()
        });
        // `status` and `signal_poll` go side by side too. Run one after the other, a
        // wpa_supplicant that has stopped answering costs both timeouts instead of one.
        let wireless: Vec<_> = wireless_names
            .iter()
            .map(|name| {
                let name = *name;
                let status =
                    scope.spawn(move || runner.output(&commands.wpa_cli, &["-i", name, "status"]));
                let signal = scope
                    .spawn(move || runner.output(&commands.wpa_cli, &["-i", name, "signal_poll"]));
                (name, status, signal)
            })
            .collect();

        let wireless: BTreeMap<&str, Wireless> = wireless
            .into_iter()
            .map(|(name, status, signal)| {
                let status = status.join().expect("wpa_cli status").unwrap_or_default();
                let signal = signal.join().expect("wpa_cli signal").unwrap_or_default();
                (name, build_wireless(&status, &signal))
            })
            .collect();
        (addrs.join().expect("ip addr worker"), wireless)
    });

    let interfaces = names
        .iter()
        .map(|(name, operstate)| {
            let (rx, tx) = counters.get(name).copied().unwrap_or((0, 0));
            Interface {
                kind: kind(config, name),
                ipv4: addrs.get(name).cloned(),
                operstate: operstate.clone(),
                wireless: wireless.get(name.as_str()).cloned(),
                rx_bytes: rx,
                tx_bytes: tx,
                name: name.clone(),
            }
        })
        .collect();

    Ok(Network { interfaces })
}

// ---- Provider ----

/// What can be asked of the network provider. It only reads, so this is the one command.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum NetworkCommand {
    Refresh,
}

impl Coalesce for NetworkCommand {
    type Key = ();

    fn key(&self) -> Self::Key {}

    fn fold(&mut self, _next: &Self) -> Fold {
        Fold::Merged
    }
}

/// Reads the network on its own thread, so `ip` and `wpa_cli` never stall the event loop.
pub struct NetworkProvider {
    pub config: NetworkConfig,
    pub commands: CommandsConfig,
    pub tick: Duration,
}

impl NetworkProvider {
    pub fn spawn(
        self,
        samples: calloop::channel::Sender<Result<Network, String>>,
    ) -> Result<ProviderHandle<NetworkCommand>, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let shutdown = Shutdown::new();
        let stop = shutdown.clone();
        let join = std::thread::Builder::new()
            .name("pippipit-network".into())
            .spawn(move || {
                let runner = SystemRunner::new(self.commands.timeout(), stop.clone());
                loop {
                    if samples
                        .send(read(&self.config, &self.commands, &runner))
                        .is_err()
                    {
                        return; // the event loop is gone
                    }
                    let deadline = Instant::now() + self.tick;
                    // Refreshes that arrived while reading collapse into the read just done.
                    if wait_for_commands(&rx, &stop, deadline, COALESCE_WINDOW).is_none() {
                        return;
                    }
                }
            })
            .map_err(|e| format!("failed to spawn the network thread: {e}"))?;
        Ok(ProviderHandle::new(tx, shutdown, join))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEV: &str = include_str!("../../tests/fixtures/proc-net-dev.txt");
    const STATUS: &str = include_str!("../../tests/fixtures/wpa-status.txt");
    const SIGNAL: &str = include_str!("../../tests/fixtures/wpa-signal.txt");
    const IPADDR: &str = include_str!("../../tests/fixtures/ip-addr.json");

    #[test]
    fn reads_counters_from_proc_net_dev() {
        let m = parse_proc_net_dev(DEV);
        assert_eq!(m.get("wlan0"), Some(&(123_456_789, 987_654_321)));
        assert_eq!(m.get("dummy0"), Some(&(0, 0)));
        // The two header lines must not be counted.
        assert!(!m.contains_key("Inter-|"));
    }

    #[test]
    fn ignores_malformed_lines() {
        let m = parse_proc_net_dev("h1\nh2\ngarbage\nfoo: 1 2\n");
        assert!(m.is_empty());
    }

    #[test]
    fn reads_wpa_status_and_signal() {
        let w = build_wireless(STATUS, SIGNAL);
        assert_eq!(w.ssid.as_deref(), Some("MyNetwork-5G"));
        assert_eq!(w.freq_mhz, Some(5180));
        assert_eq!(w.key_mgmt.as_deref(), Some("WPA2-PSK"));
        assert_eq!(w.state.as_deref(), Some("COMPLETED"));
        assert_eq!(w.rssi, Some(-49));
        assert_eq!(w.link_mbps, Some(702));
    }

    /// The cipher rides along in the same `status` output, so reading it costs nothing.
    #[test]
    fn the_cipher_comes_from_the_same_call() {
        assert_eq!(
            build_wireless(STATUS, SIGNAL).cipher.as_deref(),
            Some("CCMP")
        );
    }

    fn security_of(key_mgmt: &str, cipher: &str) -> Option<Security> {
        build_wireless(
            &format!("key_mgmt={key_mgmt}\npairwise_cipher={cipher}\n"),
            "",
        )
        .security()
    }

    /// The current suites are not worth a column: they are the network's configuration,
    /// not its state.
    #[test]
    fn the_current_suites_are_not_worth_saying() {
        for key in [
            "WPA2-PSK",
            "WPA2-EAP",
            "SAE",
            "WPA2-PSK+SAE",
            "OWE",
            "WPA2-PSK-SHA256",
        ] {
            assert_eq!(security_of(key, "CCMP"), Some(Security::Current), "{key}");
        }
    }

    /// `key_mgmt` alone cannot tell these two apart - both say `NONE`.
    #[test]
    fn the_cipher_separates_open_from_wep() {
        assert_eq!(security_of("NONE", "CCMP"), Some(Security::Open));
        assert_eq!(security_of("NONE", ""), Some(Security::Open));
        assert_eq!(security_of("NONE", "WEP-40"), Some(Security::Wep));
        assert_eq!(security_of("NONE", "WEP-104"), Some(Security::Wep));
    }

    /// An unrecognised suite is carried out as it came rather than dropped. Hiding one
    /// is how you fail to notice the one that should have worried you - and WPA1 and
    /// dynamic WEP are exactly that sort of thing.
    #[test]
    fn an_unknown_suite_is_shown_not_swallowed() {
        assert_eq!(
            security_of("WPA-PSK", "TKIP"),
            Some(Security::Unknown("WPA-PSK".into()))
        );
        assert_eq!(
            security_of("IEEE8021X", "WEP104"),
            Some(Security::Unknown("IEEE8021X".into()))
        );
        // One current suite does not excuse the other.
        assert_eq!(
            security_of("WPA2-PSK+WPA-PSK", "CCMP"),
            Some(Security::Unknown("WPA2-PSK+WPA-PSK".into()))
        );
    }

    /// Nothing to classify while disconnected.
    #[test]
    fn no_link_has_no_security() {
        assert_eq!(build_wireless("wpa_state=SCANNING\n", "").security(), None);
    }

    /// A non-ASCII SSID arrives `\xNN`-escaped from wpa_supplicant. The unescaping has
    /// its own test; this one walks it through the same path the pane uses.
    #[test]
    fn a_non_ascii_ssid_survives_the_status_parse() {
        let w = build_wireless(
            r"ssid=\xe3\x81\x95\xe3\x81\x8f\xe3\x82\x89"
                .to_string()
                .as_str(),
            "",
        );
        assert_eq!(w.ssid.as_deref(), Some("さくら"));
    }

    /// `NOISE=9999` means "unavailable". SNR is never computed, so passing it through is fine.
    #[test]
    fn noise_is_not_interpreted() {
        let p = parse_wpa_kv(SIGNAL);
        assert_eq!(p.get("NOISE").map(String::as_str), Some("9999"));
    }

    #[test]
    fn missing_wpa_output_yields_all_none() {
        let w = build_wireless("", "");
        assert_eq!(w.ssid, None);
        assert_eq!(w.rssi, None);
        assert_eq!(w.state, None);
    }

    /// While disconnected it must still report the state instead of failing.
    #[test]
    fn disconnected_state_is_kept() {
        let w = build_wireless("wpa_state=DISCONNECTED\n", "");
        assert_eq!(w.state.as_deref(), Some("DISCONNECTED"));
        assert_eq!(w.ssid, None);
    }

    #[test]
    fn unescapes_utf8_ssid() {
        assert_eq!(unescape_ssid(r"\xe3\x81\x8a"), "お");
        assert_eq!(unescape_ssid("plain-ssid"), "plain-ssid");
        // A malformed sequence must not panic.
        assert_eq!(unescape_ssid(r"\xZZ"), r"\xZZ");
        assert_eq!(unescape_ssid(r"tail\x"), r"tail\x");
    }

    #[test]
    fn picks_ipv4_only() {
        let m = parse_ip_addr(IPADDR).unwrap();
        assert_eq!(m.get("wlan0").map(String::as_str), Some("192.168.1.10/24"));
        assert_eq!(m.get("lo").map(String::as_str), Some("127.0.0.1/8"));
        // An interface with no address does not appear.
        assert!(!m.contains_key("dummy0"));
    }

    fn net() -> NetworkConfig {
        NetworkConfig::default()
    }

    /// Right after boot, wpa authentication leaves it `dormant`. It must not be hidden.
    #[test]
    fn dormant_interface_is_still_shown() {
        assert!(should_show(&net(), "wlan0", "dormant"));
        assert!(should_show(&net(), "wlan0", "up"));
        assert!(should_show(&net(), "eth0", "unknown"));
        assert!(should_show(&net(), "wlan0", "testing"));
    }

    #[test]
    fn dead_interfaces_are_hidden() {
        assert!(!should_show(&net(), "dummy0", "down"));
        assert!(!should_show(&net(), "sit0", "down"));
        assert!(!should_show(&net(), "eth0", "lowerlayerdown"));
        assert!(!should_show(&net(), "eth0", "notpresent"));
        // An unreadable operstate is not shown either.
        assert!(!should_show(&net(), "eth0", ""));
    }

    #[test]
    fn loopback_is_always_hidden() {
        assert!(!should_show(&net(), "lo", "up"));
        assert!(!should_show(&net(), "lo", "unknown"));
    }

    /// An explicit list shows exactly those, ignoring operstate and exclude alike.
    #[test]
    fn explicit_interface_list_wins() {
        let mut c = net();
        c.interface = vec!["lo".into(), "wlan*".into()];
        assert!(should_show(&c, "lo", "up"));
        assert!(should_show(&c, "wlan0", "down"));
        assert!(!should_show(&c, "eth0", "up"));
    }

    #[test]
    fn exclude_matches_by_prefix() {
        let mut c = net();
        c.exclude = vec!["docker*".into(), "veth*".into()];
        assert!(!should_show(&c, "docker0", "up"));
        assert!(!should_show(&c, "veth1a2b3c", "up"));
        // The default "lo" was overridden, so lo shows up.
        assert!(should_show(&c, "lo", "unknown"));
    }

    #[test]
    fn hide_operstate_is_configurable() {
        let mut c = net();
        c.hide_operstate = vec!["unknown".into()];
        assert!(!should_show(&c, "eth0", "unknown"));
        // down, hidden by default, now shows.
        assert!(should_show(&c, "eth0", "down"));
    }

    /// The pane is four rows and the WiFi block is three of them, so ordering decides
    /// what is seen. Alphabetically `docker0` and `veth*` come first and push it off.
    #[test]
    fn hardware_sorts_ahead_of_docker_and_veth() {
        let c = net();
        let mut found: Vec<(String, String)> = ["veth1a2b3c4", "docker0", "wlp11s0", "enp5s0"]
            .iter()
            .map(|n| (n.to_string(), "up".to_string()))
            .collect();
        order(&c, &mut found);
        let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["wlp11s0", "enp5s0", "docker0", "veth1a2b3c4"]);
    }

    /// `virtual` is a priority, never a filter.
    #[test]
    fn virtual_interfaces_are_sorted_last_but_still_shown() {
        let c = net();
        assert!(is_virtual(&c, "docker0"));
        assert!(
            should_show(&c, "docker0", "up"),
            "sorting last is not hiding"
        );
    }

    /// Somebody who counts `wg0` as real hardware takes it out of the list.
    #[test]
    fn the_virtual_list_is_configurable() {
        let mut c = net();
        c.r#virtual = vec!["docker*".into()];
        let mut found: Vec<(String, String)> = ["docker0", "wg0"]
            .iter()
            .map(|n| (n.to_string(), "up".to_string()))
            .collect();
        order(&c, &mut found);
        assert_eq!(found[0].0, "wg0");
    }

    /// An explicit list is an ordering as well as a filter.
    #[test]
    fn explicit_interface_list_keeps_its_order() {
        let mut c = net();
        c.interface = vec!["docker0".into(), "wlp11s0".into()];
        let mut found: Vec<(String, String)> = ["wlp11s0", "docker0"]
            .iter()
            .map(|n| (n.to_string(), "up".to_string()))
            .collect();
        order(&c, &mut found);
        let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["docker0", "wlp11s0"],
            "the written order wins over rank"
        );
    }

    #[test]
    fn broken_ip_json_is_an_error() {
        assert!(parse_ip_addr("nope").is_err());
    }
}
