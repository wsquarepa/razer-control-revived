use std::path::Path;

const POWERCAP_DIR: &str = "/sys/class/powercap";

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// Scans `base` (a powercap directory, e.g. `/sys/class/powercap`) for the
/// first entry whose `name` file starts with `package` ("package-0" covers
/// both intel-rapl and the AMD rapl driver) and reads its `energy_uj`
/// counter. `None` when no package zone exists or its counter cannot be
/// read/parsed. The daemon runs as root, so it can read `energy_uj` even on
/// kernels that restrict it to root as a PLATYPUS side-channel mitigation;
/// this moves the read here so unprivileged clients no longer need to.
pub fn read_cpu_package_energy_uj(base: &Path) -> Option<u64> {
    let entries = std::fs::read_dir(base).ok()?;
    for entry in entries.flatten() {
        let name = read_trimmed(&entry.path().join("name"));
        if name.is_some_and(|n| n.starts_with("package")) {
            return read_trimmed(&entry.path().join("energy_uj"))?.parse().ok();
        }
    }
    None
}

/// Production entry point: reads the CPU package energy counter from the
/// real powercap sysfs tree.
pub fn read_cpu_energy() -> Option<u64> {
    read_cpu_package_energy_uj(Path::new(POWERCAP_DIR))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "razer-daemon-cpu-energy-{}-{name}",
            std::process::id()
        ));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).expect("clean fixture");
        }
        std::fs::create_dir_all(&dir).expect("create fixture");
        dir
    }

    #[test]
    fn reads_the_package_energy_counter() {
        let dir = fixture_dir("package");
        let zone = dir.join("intel-rapl:0");
        std::fs::create_dir_all(&zone).expect("mkdir zone");
        std::fs::write(zone.join("name"), "package-0\n").expect("name");
        std::fs::write(zone.join("energy_uj"), "123456789\n").expect("energy_uj");
        assert_eq!(read_cpu_package_energy_uj(&dir), Some(123_456_789));
    }

    #[test]
    fn no_package_entry_is_none() {
        let dir = fixture_dir("no-package");
        let zone = dir.join("intel-rapl:0:0");
        std::fs::create_dir_all(&zone).expect("mkdir zone");
        std::fs::write(zone.join("name"), "core\n").expect("name");
        std::fs::write(zone.join("energy_uj"), "42\n").expect("energy_uj");
        assert_eq!(read_cpu_package_energy_uj(&dir), None);
    }

    #[test]
    fn missing_powercap_dir_is_none() {
        let dir = fixture_dir("missing").join("does-not-exist");
        assert_eq!(read_cpu_package_energy_uj(&dir), None);
    }
}
