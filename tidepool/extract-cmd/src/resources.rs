//! Observed capacity for the existing compiler daemon; no cross-process registry.
use std::fs;
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
            if let Some(limit) = read(directory, "cpu.max").and_then(|text| cpu_quota(&text)) {
                result.cpus = result.cpus.min(limit);
            }
            if let Some(limit) =
                read(directory, "cpuset.cpus.effective").and_then(|text| cpuset_count(&text))
            {
                result.cpus = result.cpus.min(limit);
            }
            if let Some(maximum) =
                read(directory, "memory.max").and_then(|text| finite_limit(&text))
            {
                // A known limit with unreadable usage cannot establish headroom.
                let current =
                    read(directory, "memory.current").and_then(|text| finite_limit(&text));
                let remaining = current
                    .map(|current| maximum.saturating_sub(current) / MB)
                    .unwrap_or(0)
                    .saturating_sub(CGROUP_HEADROOM_MB);
                result.memory_mb = result.memory_mb.min(remaining);
            }
        }
    }
    result
}

fn read(directory: &Path, name: &str) -> Option<String> {
    fs::read_to_string(directory.join(name)).ok()
}

fn cgroup_directory() -> Option<(PathBuf, PathBuf)> {
    let membership = fs::read_to_string("/proc/self/cgroup").ok()?;
    let member = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?;
    let mounts = fs::read_to_string("/proc/self/mountinfo").ok()?;
    mounts.lines().find_map(|line| {
        let (details, filesystem) = line.split_once(" - ")?;
        if filesystem.split_whitespace().next()? != "cgroup2" {
            return None;
        }
        let fields: Vec<_> = details.split_whitespace().collect();
        let root = PathBuf::from(unescape_mount(fields.get(3)?));
        let mount = PathBuf::from(unescape_mount(fields.get(4)?));
        let suffix = Path::new(member).strip_prefix(root).ok()?;
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

fn finite_limit(text: &str) -> Option<u64> {
    text.trim().parse().ok()
}

fn cpu_quota(text: &str) -> Option<usize> {
    let mut fields = text.split_whitespace();
    let quota: u64 = fields.next()?.parse().ok()?;
    let period: u64 = fields.next()?.parse().ok()?;
    if period == 0 || fields.next().is_some() {
        return None;
    }
    // A fractional CPU still needs one capability; rounding down larger grants
    // avoids allocating two cores to a 1.5-core quota.
    Some(usize::try_from((quota / period).max(u64::from(quota != 0))).ok()?)
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
    fn limits_preserve_zero_and_reject_malformed_evidence() {
        assert_eq!(finite_limit("max"), None);
        assert_eq!(finite_limit("0"), Some(0));
        assert_eq!(cpu_quota("max 100000"), None);
        assert_eq!(cpu_quota("150000 100000"), Some(1));
        assert_eq!(cpu_quota("400000 100000"), Some(4));
        assert_eq!(cpu_quota("0 100000"), Some(0));
        assert_eq!(cpu_quota("1 0"), None);
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
                        .min(cpu_quota(&format!("{} 100000", quota * 100000)).unwrap())
                        .min(cpuset_count(&format!("0-{}", child - 1)).unwrap());
                    assert_eq!(capacity, parent.min(quota).min(child));
                }
            }
        }
    }
}
