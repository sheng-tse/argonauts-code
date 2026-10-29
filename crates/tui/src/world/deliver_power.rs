//! How the computer is powered: Low Power Mode and the battery, as macOS's `pmset` says or as
//! Linux's power supplies and platform profile read. A reading takes a moment, so it runs in the
//! background, at most every half minute and only while the world asks, and the last one
//! answers meanwhile.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::clock::{Power, PowerSource};

/// How long a reading answers before the next one starts.
const READ_EVERY: Duration = Duration::from_secs(30);

/// The system's power, read in the background.
#[derive(Debug, Default, Clone)]
pub struct SystemPower {
    reading: Arc<Mutex<Reading>>,
}

#[derive(Debug, Default)]
struct Reading {
    power: Power,
    read_at: Option<Instant>,
    busy: bool,
}

impl PowerSource for SystemPower {
    fn power(&self, now: Instant) -> Power {
        let Ok(mut reading) = self.reading.lock() else {
            return Power::default();
        };
        let due = reading
            .read_at
            .is_none_or(|at| now.saturating_duration_since(at) >= READ_EVERY);
        if due && !reading.busy && !cfg!(test) {
            reading.busy = true;
            let shared = Arc::clone(&self.reading);
            let started = std::thread::Builder::new()
                .name("power".to_owned())
                .spawn(move || {
                    let power = read();
                    if let Ok(mut reading) = shared.lock() {
                        reading.power = power;
                        reading.read_at = Some(Instant::now());
                        reading.busy = false;
                    }
                });
            if started.is_err() {
                reading.busy = false;
                reading.read_at = Some(now);
            }
        }
        reading.power
    }
}

#[cfg(target_os = "macos")]
fn read() -> Power {
    let output = |args: &[&str]| {
        std::process::Command::new("/usr/bin/pmset")
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
            .unwrap_or_default()
    };
    Power {
        low_power: low_power_in_pmset(&output(&["-g"])),
        on_battery: battery_in_pmset(&output(&["-g", "batt"])),
    }
}

#[cfg(target_os = "linux")]
fn read() -> Power {
    let text = |path: &std::path::Path| {
        std::fs::read_to_string(path)
            .map(|text| text.trim().to_owned())
            .unwrap_or_default()
    };
    let supplies = std::fs::read_dir("/sys/class/power_supply")
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| {
                    let path = entry.path();
                    (text(&path.join("type")), text(&path.join("online")))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Power {
        low_power: text(std::path::Path::new("/sys/firmware/acpi/platform_profile")) == "low-power",
        on_battery: battery_in_supplies(&supplies),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn read() -> Power {
    Power::default()
}

/// `pmset -g` lists `lowpowermode 1` while Low Power Mode is on; newer Macs say `powermode 1`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn low_power_in_pmset(settings: &str) -> bool {
    settings.lines().any(|line| {
        let mut words = line.split_whitespace();
        matches!(
            (words.next(), words.next()),
            (Some("lowpowermode" | "powermode"), Some("1"))
        )
    })
}

/// `pmset -g batt` starts with where the power comes from.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn battery_in_pmset(battery: &str) -> bool {
    battery
        .lines()
        .next()
        .is_some_and(|line| line.contains("'Battery Power'"))
}

/// On battery when a mains supply says it is off line. Each supply is its type and whether it
/// is on line, as `/sys/class/power_supply` names them.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn battery_in_supplies(supplies: &[(String, String)]) -> bool {
    let mains: Vec<&str> = supplies
        .iter()
        .filter(|(kind, _)| kind == "Mains")
        .map(|(_, online)| online.as_str())
        .collect();
    let battery = supplies.iter().any(|(kind, _)| kind == "Battery");
    battery && !mains.is_empty() && mains.iter().all(|online| *online == "0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pmset_names_low_power_mode_by_either_word() {
        let laptop = "System-wide power settings:\nCurrently in use:\n standby              1\n \
                      lowpowermode         1\n sleep                1\n";
        assert!(low_power_in_pmset(laptop));
        assert!(low_power_in_pmset(" powermode            1\n"));
        assert!(!low_power_in_pmset(
            " lowpowermode         0\n powermode 2\n"
        ));
        assert!(!low_power_in_pmset(""));
    }

    #[test]
    fn pmset_says_first_where_the_power_comes_from() {
        assert!(battery_in_pmset(
            "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t81%; discharging;\n"
        ));
        assert!(!battery_in_pmset(
            "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t100%; charged;\n"
        ));
        assert!(!battery_in_pmset(""));
    }

    #[test]
    fn linux_is_on_battery_only_when_every_mains_supply_is_off_line() {
        let supply = |kind: &str, online: &str| (kind.to_owned(), online.to_owned());
        assert!(battery_in_supplies(&[
            supply("Mains", "0"),
            supply("Battery", "")
        ]));
        assert!(!battery_in_supplies(&[
            supply("Mains", "1"),
            supply("Battery", "")
        ]));
        // A desktop has no battery, and a supply list that names no mains can't tell.
        assert!(!battery_in_supplies(&[supply("Mains", "0")]));
        assert!(!battery_in_supplies(&[supply("Battery", "")]));
        assert!(!battery_in_supplies(&[]));
    }

    #[test]
    fn the_tests_never_read_the_machine() {
        let power = SystemPower::default();
        assert_eq!(power.power(Instant::now()), Power::default());
        assert!(!power.reading.lock().unwrap().busy);
    }
}
