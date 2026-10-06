//! Observed capacity for the existing compiler daemon; no cross-process registry.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const MB: u64 = 1024 * 1024;
const HOST_HEADROOM_MB: u64 = 10 * 1024;
const CGROUP_HEADROOM_MB: u64 = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ResourceCapacity {
    pub cpus: usize,
    /// Additional memory available after existing resident processes and headroom.
    pub memory_mb: u64,
}

pub(crate) fn capacity() -> ResourceCapacity {
    let cpus = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    let memory_mb = fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| mem_available_mb(&text))
        .map(|mb| mb.saturating_sub(HOST_HEADROOM_MB))
        .unwrap_or(2 * 1024);
    let mut result = ResourceCapacity { cpus, memory_mb };
    if let Some((mount, current)) = cgroup_directory() {
        for directory in current
            .ancestors()
            .take_while(|path| path.starts_with(&mount))
        {
            match read_control(directory, "cpu.max") {
                ControlFile::Value(text) => match cpu_quota(&text) {
                    Ok(Some(limit)) => result.cpus = result.cpus.min(limit),
                    Ok(None) => {}
                    Err(()) => result.cpus = 0,
                },
                ControlFile::Unreadable => result.cpus = 0,
                ControlFile::Unavailable => {}
            }
            match read_control(directory, "cpuset.cpus.effective") {
                ControlFile::Value(text) => {
                    result.cpus = result.cpus.min(cpuset_count(&text).unwrap_or(0))
                }
                ControlFile::Unreadable => result.cpus = 0,
                ControlFile::Unavailable => {}
            }
            if let Some(remaining) = memory_headroom(read_control(directory, "memory.max"), || {
                read_control(directory, "memory.current")
            }) {
                result.memory_mb = result.memory_mb.min(remaining);
            }
        }
    }
    result
}

enum ControlFile {
    Value(String),
    Unavailable,
    Unreadable,
}

fn read_control(directory: &Path, name: &str) -> ControlFile {
    let result = fs::read_to_string(directory.join(name));
    let absent_in_existing_directory = result
        .as_ref()
        .err()
        .is_some_and(|error| error.kind() == io::ErrorKind::NotFound)
        && directory.is_dir();
    control_read(result, absent_in_existing_directory)
}

fn control_read(result: io::Result<String>, directory_exists: bool) -> ControlFile {
    match result {
        Ok(value) => ControlFile::Value(value),
        // The cgroup root and disabled controllers can omit a control file.
        // A missing directory does not establish an unconstrained controller.
        Err(error) if error.kind() == io::ErrorKind::NotFound && directory_exists => {
            ControlFile::Unavailable
        }
        Err(_) => ControlFile::Unreadable,
    }
}

fn memory_headroom(maximum: ControlFile, current: impl FnOnce() -> ControlFile) -> Option<u64> {
    let maximum = match maximum {
        ControlFile::Unavailable => return None,
        ControlFile::Unreadable => return Some(0),
        ControlFile::Value(text) if text.trim() == "max" => return None,
        ControlFile::Value(text) => match text.trim().parse::<u64>() {
            Ok(value) => value,
            Err(_) => return Some(0),
        },
    };
    match current() {
        ControlFile::Value(text) => Some(
            text.trim()
                .parse::<u64>()
                .map(|current| {
                    (maximum.saturating_sub(current) / MB).saturating_sub(CGROUP_HEADROOM_MB)
                })
                .unwrap_or(0),
        ),
        ControlFile::Unavailable | ControlFile::Unreadable => Some(0),
    }
}

fn cgroup_directory() -> Option<(PathBuf, PathBuf)> {
    let membership = fs::read_to_string("/proc/self/cgroup").ok()?;
    let mounts = fs::read_to_string("/proc/self/mountinfo").ok()?;
    cgroup_directory_from(&membership, &mounts)
}

fn cgroup_directory_from(membership: &str, mounts: &str) -> Option<(PathBuf, PathBuf)> {
    let member = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?;
    if Path::new(member)
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return None;
    }
    mounts.lines().find_map(|line| {
        let (details, filesystem) = line.split_once(" - ")?;
        if filesystem.split_whitespace().next()? != "cgroup2" {
            return None;
        }
        let fields: Vec<_> = details.split_whitespace().collect();
        let root = PathBuf::from(unescape_mount(fields.get(3)?));
        let mount = PathBuf::from(unescape_mount(fields.get(4)?));
        // Membership is relative to a cgroup namespace. A mount can retain
        // its host-side root while /proc/self/cgroup names the namespace root.
        let suffix = Path::new(member)
            .strip_prefix(&root)
            .or_else(|_| Path::new(member).strip_prefix("/"))
            .ok()?;
        Some((mount.clone(), mount.join(suffix)))
    })
}

fn unescape_mount(text: &str) -> String {
    text.replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

fn mem_available_mb(text: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let values: Vec<_> = line.split_whitespace().collect();
        if values.first()? != &"MemAvailable:" || values.get(2)? != &"kB" {
            return None;
        }
        Some(values.get(1)?.parse::<u64>().ok()? / 1024)
    })
}

fn cpu_quota(text: &str) -> Result<Option<usize>, ()> {
    let mut fields = text.split_whitespace();
    let quota = fields.next().ok_or(())?;
    let period: u64 = fields.next().ok_or(())?.parse().map_err(|_| ())?;
    if period == 0 || fields.next().is_some() {
        return Err(());
    }
    if quota == "max" {
        return Ok(None);
    }
    let quota: u64 = quota.parse().map_err(|_| ())?;
    // A fractional CPU still needs one capability; rounding down larger grants
    // avoids allocating two cores to a 1.5-core quota.
    Ok(Some(
        usize::try_from((quota / period).max(u64::from(quota != 0))).map_err(|_| ())?,
    ))
}

fn cpuset_count(text: &str) -> Option<usize> {
    let mut ranges = Vec::new();
    for part in text.trim().split(',') {
        let (first, last) = match part.split_once('-') {
            Some((first, last)) => (first.parse::<usize>().ok()?, last.parse::<usize>().ok()?),
            None => {
                let value = part.parse::<usize>().ok()?;
                (value, value)
            }
        };
        if first > last {
            return None;
        }
        ranges.push((first, last));
    }
    ranges.sort_unstable();
    if ranges.windows(2).any(|pair| pair[0].1 >= pair[1].0) {
        return None;
    }
    ranges.into_iter().try_fold(0usize, |count, (first, last)| {
        count.checked_add(last.checked_sub(first)?.checked_add(1)?)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cgroup_mount_roots_and_namespace_membership_select_actual_ancestors() {
        let host_mount = "31 25 0:27 / /sys/fs/cgroup rw - cgroup2 cgroup rw\n";
        let (mount, current) =
            cgroup_directory_from("0::/user.slice/worker\n", host_mount).unwrap();
        assert_eq!(mount, Path::new("/sys/fs/cgroup"));
        assert_eq!(current, Path::new("/sys/fs/cgroup/user.slice/worker"));
        let ancestors: Vec<_> = current
            .ancestors()
            .take_while(|path| path.starts_with(&mount))
            .collect();
        assert_eq!(
            ancestors,
            [
                Path::new("/sys/fs/cgroup/user.slice/worker"),
                Path::new("/sys/fs/cgroup/user.slice"),
                Path::new("/sys/fs/cgroup")
            ]
        );
        let namespace_mount = "31 25 0:27 /docker/owner /sys/fs/cgroup rw - cgroup2 cgroup rw\n";
        assert_eq!(
            cgroup_directory_from("0::/task\n", namespace_mount)
                .unwrap()
                .1,
            Path::new("/sys/fs/cgroup/task")
        );
        let escaped_mount = r"31 25 0:27 /owner /some\040path rw - cgroup2 cgroup rw";
        assert_eq!(
            cgroup_directory_from("0::/owner/task\n", escaped_mount)
                .unwrap()
                .1,
            Path::new("/some path/task")
        );
        assert!(cgroup_directory_from("0::/../outside\n", host_mount).is_none());
    }

    #[test]
    fn unreadable_installed_controls_refuse_memory_while_max_and_absence_remain_unbounded() {
        let value = |text: &str| ControlFile::Value(text.to_owned());
        assert_eq!(
            memory_headroom(value("max"), || panic!(
                "unbounded limit must not read usage"
            )),
            None
        );
        assert_eq!(
            memory_headroom(ControlFile::Unavailable, || panic!(
                "absent controller must not read usage"
            )),
            None
        );
        assert_eq!(
            memory_headroom(ControlFile::Unreadable, || panic!(
                "unreadable limit already refuses"
            )),
            Some(0)
        );
        assert_eq!(memory_headroom(value("malformed"), || value("0")), Some(0));
        assert_eq!(memory_headroom(value("0"), || value("0")), Some(0));
        let maximum = (8 * 1024 * MB).to_string();
        let current = (3 * 1024 * MB).to_string();
        assert_eq!(
            memory_headroom(value(&maximum), || value(&current)),
            Some(4 * 1024)
        );
        assert_eq!(
            memory_headroom(value(&maximum), || ControlFile::Unreadable),
            Some(0)
        );
        assert_eq!(
            memory_headroom(value(&maximum), || ControlFile::Unavailable),
            Some(0)
        );
        assert_eq!(memory_headroom(value(&maximum), || value("max")), Some(0));
        assert!(matches!(
            control_read(Err(io::ErrorKind::PermissionDenied.into()), true),
            ControlFile::Unreadable
        ));
        assert!(matches!(
            control_read(Err(io::ErrorKind::InvalidData.into()), true),
            ControlFile::Unreadable
        ));
        assert!(matches!(
            control_read(Err(io::ErrorKind::NotFound.into()), true),
            ControlFile::Unavailable
        ));
        assert!(matches!(
            control_read(Err(io::ErrorKind::NotFound.into()), false),
            ControlFile::Unreadable
        ));
    }

    #[test]
    fn limits_preserve_zero_and_reject_malformed_evidence() {
        assert_eq!(cpu_quota("max 100000"), Ok(None));
        assert_eq!(cpu_quota("150000 100000"), Ok(Some(1)));
        assert_eq!(cpu_quota("400000 100000"), Ok(Some(4)));
        assert_eq!(cpu_quota("0 100000"), Ok(Some(0)));
        assert_eq!(cpu_quota("1 0"), Err(()));
        assert_eq!(cpu_quota("malformed 100000"), Err(()));
        assert_eq!(cpuset_count("0-3,8,10-11\n"), Some(7));
        assert_eq!(cpuset_count("0-3,3"), None);
        assert_eq!(cpuset_count("5-2"), None);
        assert_eq!(
            mem_available_mb("MemTotal: 999999 kB\nMemAvailable: 4096 kB\n"),
            Some(4)
        );
    }
    #[test]
    fn cpu_and_cpuset_constraints_never_expand_parent_capacity() {
        for parent in 1..=32 {
            for quota in 1..=32 {
                for child in 1..=32 {
                    let capacity = parent
                        .min(
                            cpu_quota(&format!("{} 100000", quota * 100000))
                                .unwrap()
                                .unwrap(),
                        )
                        .min(cpuset_count(&format!("0-{}", child - 1)).unwrap());
                    assert_eq!(capacity, parent.min(quota).min(child));
                }
            }
        }
    }
}
