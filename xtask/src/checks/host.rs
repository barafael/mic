//! What the host does besides the box, over a check: its CPU time busy, from /proc/stat, less
//! the box's, from its pods' cgroups. What a check measures (frame rates, CPU, a voice under
//! loss) isn't relied on when others loaded the host (docs/review-0.1.0.md: the video phase
//! measured 94 % of a core beside another box, 56 % quiet). The box's own load doesn't count:
//! room-40 loads the host itself, and that is what it measures.

use std::path::PathBuf;
use std::time::Instant;

use crate::podman::{TestBox, podman};
use crate::process;

/// The share of the host's cores others may take before a measurement isn't relied on.
const BUSY: f64 = 0.25;

/// Linux's clock ticks in /proc/stat, a hundredth of a second.
const TICKS: f64 = 100.0;

/// The host and the box at a moment, to be compared with a later one.
pub struct Host {
    at: Instant,
    /// The host's CPU time busy so far, in seconds.
    busy: f64,
    /// The box's pods' cgroups, and the CPU time they've used so far, in seconds.
    pods: Vec<PathBuf>,
    ours: f64,
}

impl Host {
    pub fn now(the_box: &TestBox) -> Self {
        let pods = pod_cgroups(the_box);
        Self {
            at: Instant::now(),
            busy: host_busy().unwrap_or(0.0),
            ours: pods.iter().filter_map(|pod| cgroup_cpu(pod)).sum(),
            pods,
        }
    }

    /// How many cores others used on average since, and whether that's too many.
    pub fn since(&self) -> Others {
        let seconds = self.at.elapsed().as_secs_f64().max(f64::EPSILON);
        let busy = host_busy().unwrap_or(self.busy) - self.busy;
        let ours = self
            .pods
            .iter()
            .filter_map(|pod| cgroup_cpu(pod))
            .sum::<f64>()
            - self.ours;
        Others {
            cores: ((busy - ours) / seconds).max(0.0),
            of: std::thread::available_parallelism().map_or(1, usize::from) as f64,
        }
    }
}

/// What others used of the host, in cores, of how many it has.
pub struct Others {
    pub cores: f64,
    pub of: f64,
}

impl Others {
    pub fn busy(&self) -> bool {
        self.cores > self.of * BUSY
    }
}

impl std::fmt::Display for Others {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "others used {:.1} of the host's {:.0} cores",
            self.cores, self.of
        )
    }
}

/// The host's CPU time busy so far: all but idle and waiting on I/O.
fn host_busy() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let ticks: Vec<f64> = stat
        .lines()
        .next()?
        .strip_prefix("cpu ")?
        .split_whitespace()
        .filter_map(|field| field.parse().ok())
        .collect();
    // user nice system idle iowait irq softirq steal: guest time is in user's already.
    let all: f64 = ticks.iter().take(8).sum();
    let idle = ticks.get(3)? + ticks.get(4)?;
    Some((all - idle) / TICKS)
}

/// The cgroup of each of the box's pods: its containers' parent, which outlives a container's
/// restart, where the container's own cgroup starts again from nothing.
fn pod_cgroups(the_box: &TestBox) -> Vec<PathBuf> {
    let containers = the_box.containers();
    if containers.is_empty() {
        return Vec::new();
    }
    let mut argv = vec!["inspect", "--format", "{{.State.CgroupPath}}"];
    argv.extend(containers.iter().map(String::as_str));
    let mut pods: Vec<PathBuf> = process::output(podman(&argv))
        .unwrap_or_default()
        .lines()
        .filter_map(|path| {
            let path = path.trim().trim_start_matches('/');
            (!path.is_empty()).then(|| PathBuf::from("/sys/fs/cgroup").join(path))
        })
        .filter_map(|container| container.parent().map(PathBuf::from))
        .collect();
    pods.sort();
    pods.dedup();
    pods
}

/// The CPU time a cgroup and those under it have used, in seconds.
fn cgroup_cpu(cgroup: &std::path::Path) -> Option<f64> {
    let stat = std::fs::read_to_string(cgroup.join("cpu.stat")).ok()?;
    let usec: f64 = stat
        .lines()
        .find_map(|line| line.strip_prefix("usage_usec "))?
        .trim()
        .parse()
        .ok()?;
    Some(usec / 1e6)
}

/// Linux's /proc/stat, as the box is Linux's; xtask builds elsewhere too.
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn the_host_is_busy_for_as_long_as_it_works() {
        let before = host_busy().expect("/proc/stat says how busy the host has been");
        let started = Instant::now();
        while started.elapsed().as_millis() < 300 {
            std::hint::black_box(0u64);
        }
        assert!(host_busy().unwrap() > before);
    }
}
