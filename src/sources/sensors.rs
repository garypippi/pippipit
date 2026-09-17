//! Temperature sensors, from a full scan of `/sys/class/hwmon`.
//!
//! **The hwmon numbers change from boot to boot** (a complete reshuffle was observed).
//! Paths never go into the config; everything resolves through `name` and `tempN_label`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::SensorsConfig;

const HWMON_DIR: &str = "/sys/class/hwmon";

/// The order used for an automatic scan, from `SensorsConfig::group_order`.
///
/// Because the hwmon numbers change from boot to boot,
/// emitting the scan order verbatim would **reorder the display every boot**. Sorting by group keeps it stable.
fn group_rank(config: &SensorsConfig, group: &str) -> usize {
    config
        .group_order
        .iter()
        .position(|g| g == group)
        .unwrap_or(config.group_order.len())
}

/// One sensor's reading from a single scan.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// `<hwmon name>/<temp label>`, or just `<hwmon name>` when there is no label.
    pub key: String,
    /// How it is grouped on screen (`CPU`, `GPU`, `MB`, …).
    pub group: String,
    /// The label as displayed.
    pub label: String,
    pub celsius: f32,
    pub warn: f32,
    pub crit: f32,
}

impl Reading {
    pub fn is_crit(&self) -> bool {
        self.celsius >= self.crit
    }

    pub fn is_warn(&self) -> bool {
        !self.is_crit() && self.celsius >= self.warn
    }
}

/// Turn an hwmon chip name into a human-facing heading.
///
/// Resolution order: the config's `group_map`, then the built-in table, then the chip name as-is.
/// An unknown name passes through unchanged, so an unfamiliar system never breaks it.
fn friendly_group(config: &SensorsConfig, hwmon_name: &str) -> String {
    if let Some(group) = lookup_pattern(&config.group_map, hwmon_name) {
        return group;
    }
    builtin_group(hwmon_name)
}

/// Look a name up in one of the config's override tables: exact match first, then a
/// trailing-`*` prefix match.
///
/// When two prefixes both match, the longer (more specific) pattern wins.
fn lookup_pattern(table: &BTreeMap<String, String>, name: &str) -> Option<String> {
    if let Some(value) = table.get(name) {
        return Some(value.clone());
    }
    table
        .iter()
        .filter_map(|(pattern, value)| {
            let prefix = pattern.strip_suffix('*')?;
            name.starts_with(prefix).then_some((prefix.len(), value))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, value)| value.clone())
}

/// The built-in chip-name-to-group table.
fn builtin_group(hwmon_name: &str) -> String {
    match hwmon_name {
        "k10temp" | "coretemp" | "zenpower" => "CPU",
        "amdgpu" | "nouveau" | "i915" => "GPU",
        "nvme" => "NVMe",
        "asusec" | "asus_ec_sensors" | "nct6775" => "MB",
        other if other.starts_with("mt7921") || other.starts_with("iwlwifi") => "WiFi",
        other => other,
    }
    .to_string()
}

/// Turn a raw sysfs temperature label into something readable.
///
/// `Tctl`, `junction` and `Composite` are the kernel's names, not anybody's: they say
/// what the driver calls the sensor rather than what it measures. Resolution order is
/// the config's `label_map`, then the built-in table, then the raw label unchanged -
/// an unfamiliar chip keeps whatever it reports rather than losing its name.
///
/// **Only the display name changes.** The key stays raw, so `[[sensors.entry]]`,
/// `sparkline` and `label_map` are all still written against the same identifier.
fn friendly_label(config: &SensorsConfig, key: &str, raw: String) -> String {
    if let Some(label) = lookup_pattern(&config.label_map, key) {
        return label;
    }
    builtin_label(key).map(str::to_string).unwrap_or(raw)
}

/// The built-in key-to-label table.
///
/// Deliberately short. A guess that renames a sensor into something it is not is worse
/// than the raw name, so this covers the ones whose meaning is unambiguous and leaves
/// everything else alone.
fn builtin_label(key: &str) -> Option<&'static str> {
    Some(match key {
        // AMD's control temperature: the die reading the firmware acts on.
        "k10temp/Tctl" => "Package",
        "amdgpu/edge" => "Edge",
        // The hottest spot on the die. AMD's own tooling calls it the hotspot.
        "amdgpu/junction" => "Hotspot",
        "amdgpu/mem" => "VRAM",
        // The drive-wide composite of every NVMe sensor.
        "nvme/Composite" => "Drive",
        other if other.starts_with("k10temp/Tccd") => "Die",
        other if other.starts_with("coretemp/Package") => "Package",
        // These chips report no label at all, so the key is the bare chip name.
        other if other.starts_with("mt7921") || other.starts_with("iwlwifi") => "Adapter",
        _ => return None,
    })
}

/// Read all of `/sys/class/hwmon`.
///
/// A sensor that fails to read is skipped rather than propagated.
/// This is a resident TUI; one unreadable sensor must not take the whole thing down.
pub fn read_all(config: &SensorsConfig) -> std::io::Result<Vec<Reading>> {
    let mut found = Vec::new();

    let mut chips: Vec<PathBuf> = std::fs::read_dir(HWMON_DIR)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .collect();
    // The numbering cannot be trusted, but the scan order itself should at least be stable.
    chips.sort();

    for chip in chips {
        let Some(name) = read_trimmed(&chip.join("name")) else {
            continue;
        };
        let mut inputs: Vec<PathBuf> = match std::fs::read_dir(&chip) {
            Ok(entries) => entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| is_temp_input(p))
                .collect(),
            Err(_) => continue,
        };
        inputs.sort();

        for input in inputs {
            let Some(millicelsius) = read_trimmed(&input).and_then(|s| s.parse::<i64>().ok())
            else {
                continue;
            };
            let celsius = millicelsius as f32 / 1000.0;
            let prefix = strip_suffix_path(&input, "_input");
            let label = read_trimmed(&PathBuf::from(format!("{prefix}_label")));

            let key = match &label {
                Some(l) => format!("{name}/{l}"),
                None => name.clone(),
            };
            let (warn, crit) = resolve_thresholds(config, &key, &prefix);

            let label = friendly_label(config, &key, label.unwrap_or_else(|| default_label(&name)));
            found.push(Reading {
                group: friendly_group(config, &name),
                label,
                key,
                celsius,
                warn,
                crit,
            });
        }
    }

    Ok(apply_config(config, found))
}

/// How thresholds resolve.
///
/// 1. an explicit value in the config
/// 2. sysfs `_crit` / `_max`
/// 3. with only `_crit`, warn becomes crit * 0.85
/// 4. the config's fallback
fn resolve_thresholds(config: &SensorsConfig, key: &str, prefix: &str) -> (f32, f32) {
    let entry = config.entry.iter().find(|e| e.key == key);

    let sysfs_crit = read_millicelsius(&format!("{prefix}_crit"));
    let sysfs_max = read_millicelsius(&format!("{prefix}_max"));

    let crit = entry
        .and_then(|e| e.crit)
        .or(sysfs_crit)
        .unwrap_or(config.crit);

    let warn = entry
        .and_then(|e| e.warn)
        .or(sysfs_max)
        .or_else(|| sysfs_crit.map(|c| c * 0.85))
        .unwrap_or(config.warn);

    (warn, crit)
}

/// Apply the config: filtering, ordering and the cap.
fn apply_config(config: &SensorsConfig, mut found: Vec<Reading>) -> Vec<Reading> {
    if config.hide_zero {
        // An unconnected sensor header reports exactly 0.0°C.
        found.retain(|r| r.celsius != 0.0);
    }

    if config.entry.is_empty() {
        // Give it a stable order; ties keep the scan order, since sort_by is stable.
        found.sort_by(|a, b| {
            group_rank(config, &a.group)
                .cmp(&group_rank(config, &b.group))
                .then_with(|| a.group.cmp(&b.group))
        });
        found.truncate(config.max);
        return found;
    }

    // Order by the config's list, pushing anything unlisted to the back.
    let mut ordered = Vec::with_capacity(found.len());
    for entry in &config.entry {
        if let Some(pos) = found.iter().position(|r| r.key == entry.key) {
            let mut reading = found.remove(pos);
            if let Some(label) = &entry.label {
                reading.label = label.clone();
            }
            if let Some(group) = &entry.group {
                reading.group = group.clone();
            }
            ordered.push(reading);
        }
    }
    ordered.extend(found);
    ordered.truncate(config.max);
    ordered
}

fn is_temp_input(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.starts_with("temp")
        && name.ends_with("_input")
        && name["temp".len()..name.len() - "_input".len()]
            .chars()
            .all(|c| c.is_ascii_digit())
}

/// A chip with no `tempN_label` (some Wi-Fi chips, say) uses its chip name as the label.
/// That says more than `temp1` would.
fn default_label(chip_name: &str) -> String {
    chip_name.to_string()
}

fn strip_suffix_path(path: &Path, suffix: &str) -> String {
    let s = path.to_string_lossy().into_owned();
    s.strip_suffix(suffix).map(str::to_string).unwrap_or(s)
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn read_millicelsius(path: &str) -> Option<f32> {
    read_trimmed(Path::new(path))
        .and_then(|s| s.parse::<i64>().ok())
        .map(|v| v as f32 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SensorEntry;

    fn cfg() -> SensorsConfig {
        SensorsConfig::default()
    }

    fn reading(key: &str, celsius: f32) -> Reading {
        Reading {
            key: key.to_string(),
            group: "X".into(),
            label: key.into(),
            celsius,
            warn: 75.0,
            crit: 90.0,
        }
    }

    #[test]
    fn friendly_group_maps_known_chips() {
        let c = cfg();
        assert_eq!(friendly_group(&c, "k10temp"), "CPU");
        assert_eq!(friendly_group(&c, "amdgpu"), "GPU");
        assert_eq!(friendly_group(&c, "asusec"), "MB");
        assert_eq!(friendly_group(&c, "mt7921_phy0"), "WiFi");
        assert_eq!(friendly_group(&c, "nvme"), "NVMe");
    }

    /// `Tctl`, `junction` and `Composite` say what the driver calls the sensor, not
    /// what it measures.
    #[test]
    fn raw_sysfs_labels_get_readable_names() {
        let c = cfg();
        let name = |key: &str, raw: &str| friendly_label(&c, key, raw.to_string());
        assert_eq!(name("k10temp/Tctl", "Tctl"), "Package");
        assert_eq!(name("amdgpu/edge", "edge"), "Edge");
        assert_eq!(name("amdgpu/junction", "junction"), "Hotspot");
        assert_eq!(name("amdgpu/mem", "mem"), "VRAM");
        assert_eq!(name("nvme/Composite", "Composite"), "Drive");
        // These chips report no label, so the key is the bare chip name.
        assert_eq!(name("mt7921_phy0", "mt7921_phy0"), "Adapter");
        assert_eq!(name("k10temp/Tccd1", "Tccd1"), "Die");
    }

    /// A guess that renames a sensor into something it is not is worse than the raw
    /// name, so anything the table does not know keeps what it reports.
    #[test]
    fn an_unknown_label_is_left_alone() {
        let c = cfg();
        assert_eq!(friendly_label(&c, "asusec/VRM", "VRM".into()), "VRM");
        assert_eq!(
            friendly_label(&c, "weird_chip/temp1", "temp1".into()),
            "temp1"
        );
    }

    /// A config override beats the built-in table, and matches by prefix like the rest.
    #[test]
    fn label_map_overrides_the_builtin_table() {
        let mut c = cfg();
        c.label_map.insert("amdgpu/junction".into(), "Die".into());
        c.label_map.insert("nvme/Sensor*".into(), "Drive".into());
        assert_eq!(
            friendly_label(&c, "amdgpu/junction", "junction".into()),
            "Die"
        );
        assert_eq!(
            friendly_label(&c, "nvme/Sensor 2", "Sensor 2".into()),
            "Drive"
        );
        // Anything not overridden still comes from the built-in table.
        assert_eq!(friendly_label(&c, "amdgpu/mem", "mem".into()), "VRAM");
    }

    /// The key is what the rest of the config is written against, so it must stay raw.
    #[test]
    fn renaming_the_label_leaves_the_key_alone() {
        let c = cfg();
        let key = "amdgpu/junction";
        assert_eq!(friendly_label(&c, key, "junction".into()), "Hotspot");
        // A sparkline pinned by key still matches after the rename.
        let mut pinned = cfg();
        pinned.sparkline = vec![key.to_string()];
        assert!(pinned.wants_sparkline("GPU", key, false));
    }

    #[test]
    fn unknown_chip_keeps_its_name() {
        assert_eq!(friendly_group(&cfg(), "weird_chip"), "weird_chip");
    }

    /// A config override beats the built-in table.
    #[test]
    fn group_map_overrides_builtin_table() {
        let mut c = cfg();
        c.group_map.insert("k10temp".into(), "Processor".into());
        assert_eq!(friendly_group(&c, "k10temp"), "Processor");
        // Anything not overridden still comes from the built-in table.
        assert_eq!(friendly_group(&c, "amdgpu"), "GPU");
    }

    /// A chip the built-in table does not know can still be named in the config.
    #[test]
    fn group_map_names_unknown_chips() {
        let mut c = cfg();
        c.group_map.insert("weird_chip".into(), "MB".into());
        assert_eq!(friendly_group(&c, "weird_chip"), "MB");
    }

    #[test]
    fn group_map_matches_by_prefix() {
        let mut c = cfg();
        c.group_map.insert("iwlwifi*".into(), "WLAN".into());
        assert_eq!(friendly_group(&c, "iwlwifi_1"), "WLAN");
    }

    /// When prefixes collide, the more specific (longer) pattern wins.
    #[test]
    fn longer_prefix_wins() {
        let mut c = cfg();
        c.group_map.insert("nvme*".into(), "Disk".into());
        c.group_map.insert("nvme_boot*".into(), "Boot".into());
        assert_eq!(friendly_group(&c, "nvme_boot0"), "Boot");
        assert_eq!(friendly_group(&c, "nvme_data0"), "Disk");
    }

    /// An exact match beats a prefix match.
    #[test]
    fn exact_match_beats_prefix() {
        let mut c = cfg();
        c.group_map.insert("nvme*".into(), "Disk".into());
        c.group_map.insert("nvme".into(), "NVMe".into());
        assert_eq!(friendly_group(&c, "nvme"), "NVMe");
    }

    #[test]
    fn group_order_is_configurable() {
        let mut c = cfg();
        c.group_order = vec!["MB".into(), "CPU".into()];
        assert!(group_rank(&c, "MB") < group_rank(&c, "CPU"));
        // A group absent from the order goes last.
        assert!(group_rank(&c, "CPU") < group_rank(&c, "GPU"));
    }

    #[test]
    fn is_temp_input_rejects_thresholds() {
        assert!(is_temp_input(Path::new("/x/temp1_input")));
        assert!(is_temp_input(Path::new("/x/temp12_input")));
        assert!(!is_temp_input(Path::new("/x/temp1_crit")));
        assert!(!is_temp_input(Path::new("/x/temp1_label")));
        assert!(!is_temp_input(Path::new("/x/in0_input")));
        assert!(!is_temp_input(Path::new("/x/tempX_input")));
    }

    /// An unconnected sensor header reports 0.0°C.
    #[test]
    fn hide_zero_drops_unconnected_sensors() {
        let mut c = cfg();
        c.hide_zero = true;
        let out = apply_config(
            &c,
            vec![reading("a", 35.0), reading("nct6775/T_Sensor", 0.0)],
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].key, "a");
    }

    #[test]
    fn hide_zero_off_keeps_them() {
        let mut c = cfg();
        c.hide_zero = false;
        let out = apply_config(&c, vec![reading("a", 35.0), reading("b", 0.0)]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn entries_define_order_and_labels() {
        let mut c = cfg();
        c.entry = vec![
            SensorEntry {
                key: "b".into(),
                group: Some("GPU".into()),
                label: Some("edge".into()),
                warn: None,
                crit: None,
            },
            SensorEntry {
                key: "a".into(),
                group: None,
                label: None,
                warn: None,
                crit: None,
            },
        ];
        let out = apply_config(
            &c,
            vec![reading("a", 1.0), reading("b", 2.0), reading("z", 3.0)],
        );
        assert_eq!(
            out.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
            ["b", "a", "z"]
        );
        assert_eq!(out[0].label, "edge");
        assert_eq!(out[0].group, "GPU");
    }

    #[test]
    fn auto_scan_order_is_stable_regardless_of_hwmon_index() {
        let c = cfg();
        // Suppose they arrive in hwmon-number order: nvme, coretemp, nct6775, i915, iwlwifi.
        let scanned = vec![
            Reading {
                group: "NVMe".into(),
                ..reading("nvme/Composite", 35.0)
            },
            Reading {
                group: "CPU".into(),
                ..reading("coretemp/Package id 0", 43.0)
            },
            Reading {
                group: "MB".into(),
                ..reading("nct6775/VRM", 47.0)
            },
            Reading {
                group: "GPU".into(),
                ..reading("i915/edge", 44.0)
            },
            Reading {
                group: "WiFi".into(),
                ..reading("iwlwifi_1", 52.0)
            },
        ];
        let out = apply_config(&c, scanned);
        assert_eq!(
            out.iter().map(|r| r.group.as_str()).collect::<Vec<_>>(),
            ["CPU", "GPU", "WiFi", "NVMe", "MB"]
        );
    }

    #[test]
    fn unknown_groups_sort_after_known_ones() {
        let c = cfg();
        let scanned = vec![
            Reading {
                group: "weird".into(),
                ..reading("weird/x", 1.0)
            },
            Reading {
                group: "CPU".into(),
                ..reading("coretemp/Package id 0", 2.0)
            },
        ];
        let out = apply_config(&c, scanned);
        assert_eq!(out[0].group, "CPU");
        assert_eq!(out[1].group, "weird");
    }

    #[test]
    fn max_truncates() {
        let mut c = cfg();
        c.max = 2;
        let out = apply_config(
            &c,
            vec![reading("a", 1.0), reading("b", 2.0), reading("c", 3.0)],
        );
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn warn_and_crit_classify() {
        let mut r = reading("a", 80.0);
        r.warn = 75.0;
        r.crit = 90.0;
        assert!(r.is_warn());
        assert!(!r.is_crit());
        r.celsius = 95.0;
        assert!(r.is_crit());
        assert!(!r.is_warn());
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use crate::config::SensorsConfig;

    /// Prints what this machine actually exposes, for eyeballing.
    ///
    /// **Ignored by default, and it must stay that way.** It reads live sensors,
    /// so its output describes this machine's hardware. Run it deliberately, and
    /// do not paste the output anywhere public:
    ///
    /// ```text
    /// cargo test -- --ignored --nocapture dump_live_sensors
    /// ```
    #[test]
    #[ignore = "reads live sensors; its output describes this machine"]
    fn dump_live_sensors() {
        let cfg = SensorsConfig::default();
        match read_all(&cfg) {
            Ok(readings) => {
                for r in &readings {
                    println!(
                        "{:<26} group={:<5} label={:<12} {:>6.1}°C  warn={:<6.1} crit={:<6.1}",
                        r.key, r.group, r.label, r.celsius, r.warn, r.crit
                    );
                }
                println!("count = {}", readings.len());
            }
            Err(e) => println!("unavailable: {e}"),
        }
    }
}
