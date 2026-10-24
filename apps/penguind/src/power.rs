//! The battery, for the rules: read from `/sys/class/power_supply`.

use std::path::Path;

use wpe_ipc::Battery;

const SUPPLIES: &str = "/sys/class/power_supply";

/// The system's battery now; None without one.
pub fn battery() -> Option<Battery> {
    read(Path::new(SUPPLIES))
}

fn read(dir: &Path) -> Option<Battery> {
    let field = |supply: &Path, name: &str| {
        std::fs::read_to_string(supply.join(name))
            .ok()
            .map(|s| s.trim().to_owned())
    };
    let mut plugged = false;
    let (mut charge, mut full) = (0u64, 0u64);
    let mut batteries = 0;
    let mut discharging = false;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let supply = entry.path();
        match field(&supply, "type").as_deref() {
            Some("Mains") | Some("USB") => {
                plugged |= field(&supply, "online").as_deref() == Some("1")
            }
            // Not a mouse's or a headset's.
            Some("Battery") if field(&supply, "scope").as_deref() != Some("Device") => {
                batteries += 1;
                discharging |= field(&supply, "status").as_deref() == Some("Discharging");
                let number = |name| field(&supply, name).and_then(|v| v.parse::<u64>().ok());
                match (number("energy_now"), number("energy_full")) {
                    (Some(now), Some(f)) => (charge, full) = (charge + now, full + f),
                    _ => {
                        if let (Some(now), Some(f)) = (number("charge_now"), number("charge_full"))
                        {
                            (charge, full) = (charge + now, full + f);
                        } else if let Some(c) = number("capacity") {
                            (charge, full) = (charge + c, full + 100);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    (batteries > 0).then(|| Battery {
        discharging: discharging && !plugged,
        percent: (full > 0).then(|| ((charge * 100 + full / 2) / full).min(100) as u32),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supply(dir: &Path, name: &str, fields: &[(&str, &str)]) {
        let d = dir.join(name);
        std::fs::create_dir_all(&d).unwrap();
        for (k, v) in fields {
            std::fs::write(d.join(k), format!("{v}\n")).unwrap();
        }
    }

    #[test]
    fn reads_batteries() {
        let dir = std::env::temp_dir().join(format!("wpe-power-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read(&dir), None);
        supply(&dir, "AC", &[("type", "Mains"), ("online", "0")]);
        supply(
            &dir,
            "BAT0",
            &[
                ("type", "Battery"),
                ("status", "Discharging"),
                ("energy_now", "30"),
                ("energy_full", "40"),
            ],
        );
        supply(
            &dir,
            "hidpp_battery_0",
            &[("type", "Battery"), ("scope", "Device"), ("capacity", "5")],
        );
        assert_eq!(
            read(&dir),
            Some(Battery {
                discharging: true,
                percent: Some(75)
            })
        );
        supply(&dir, "AC", &[("type", "Mains"), ("online", "1")]);
        assert!(!read(&dir).unwrap().discharging);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
