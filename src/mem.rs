//! How much memory the machine has free, and whether room is left for one more worker.
//!
//! One probe, shared by everything that asks (`reeve run`'s worker cap, `reeve ls`'s memory
//! line, `reeve heavy`). It reads what the OS already reports and keeps no state.

use std::process::Command;

/// What the OS says about memory right now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mem {
    /// Installed memory, in bytes.
    pub total: u64,
    /// Share of it the OS could hand out without swapping or killing anything, 0-100.
    pub free_pct: u8,
}

const GB: f64 = 1024.0 * 1024.0 * 1024.0;

impl Mem {
    pub fn total_gb(&self) -> f64 {
        self.total as f64 / GB
    }

    pub fn free_gb(&self) -> f64 {
        self.total_gb() * f64::from(self.free_pct) / 100.0
    }
}

/// The machine's memory now, or None where reeve can't tell.
pub fn probe() -> Option<Mem> {
    if cfg!(target_os = "macos") {
        let sysctl = |key: &str| {
            let o = Command::new("sysctl").args(["-n", key]).output().ok()?;
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u64>()
                .ok()
        };
        let total = sysctl("hw.memsize")?;
        // The same figure Activity Monitor's pressure graph is drawn from, as a percentage.
        let free_pct = sysctl("kern.memorystatus_level")?.min(100) as u8;
        Some(Mem { total, free_pct })
    } else {
        parse_meminfo(&std::fs::read_to_string("/proc/meminfo").ok()?)
    }
}

/// `MemTotal` and `MemAvailable` from /proc/meminfo, which are in kB.
pub fn parse_meminfo(s: &str) -> Option<Mem> {
    let kb = |key: &str| {
        s.lines().find_map(|l| {
            l.strip_prefix(key)?
                .trim_start_matches(':')
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        })
    };
    let total = kb("MemTotal")? * 1024;
    let avail = kb("MemAvailable")? * 1024;
    let free_pct = (avail.saturating_mul(100) / total.max(1)).min(100) as u8;
    Some(Mem { total, free_pct })
}

/// Memory one more worker is expected to take, in GB.
pub const GB_PER_WORKER: f64 = 0.75;
/// Below this much free memory no worker is started, in GB.
pub const DEFAULT_MIN_FREE_GB: f64 = 1.5;

/// The limits `reeve run` holds new workers to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// At most this many workers live at once.
    pub max_workers: usize,
    /// Keep at least this much free, in GB.
    pub min_free_gb: f64,
}

impl Limits {
    /// REEVE_MAX_WORKERS and REEVE_MIN_FREE (GB) override what the machine's size suggests:
    /// one worker per GB of installed memory.
    pub fn from_env(mem: Option<&Mem>) -> Limits {
        let var = |k: &str| std::env::var(k).ok();
        Limits::from(
            mem,
            var("REEVE_MAX_WORKERS").as_deref(),
            var("REEVE_MIN_FREE").as_deref(),
        )
    }

    pub fn from(mem: Option<&Mem>, max: Option<&str>, min_free: Option<&str>) -> Limits {
        Limits {
            max_workers: max
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or_else(|| mem.map_or(usize::MAX, |m| m.total_gb().floor() as usize)),
            min_free_gb: min_free
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(DEFAULT_MIN_FREE_GB),
        }
    }
}

/// Whether one more worker fits, or why not. `live` workers are running; `starting` ones are
/// not yet using what they will, so each is counted as already taking its share of what is free.
pub fn room(
    mem: Option<&Mem>,
    limits: &Limits,
    live: usize,
    starting: usize,
) -> Result<(), String> {
    if live >= limits.max_workers {
        return Err(format!(
            "{live} workers are live, at the limit of {} (REEVE_MAX_WORKERS)",
            limits.max_workers
        ));
    }
    let Some(m) = mem else { return Ok(()) };
    let free = m.free_gb() - starting as f64 * GB_PER_WORKER;
    if free - GB_PER_WORKER < limits.min_free_gb {
        return Err(format!(
            "{:.1} GB free of {:.0} GB with {live} workers live, and a worker takes about {GB_PER_WORKER} GB while {:.1} GB (REEVE_MIN_FREE) stay free",
            free.max(0.0),
            m.total_gb(),
            limits.min_free_gb
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(total_gb: u64, free_pct: u8) -> Mem {
        Mem {
            total: total_gb << 30,
            free_pct,
        }
    }

    #[test]
    fn meminfo_gives_available_over_total() {
        let s = "MemTotal:       16384000 kB\nMemFree:  100 kB\nMemAvailable:    4096000 kB\n";
        let got = parse_meminfo(s).unwrap();
        assert_eq!(got.free_pct, 25);
        assert_eq!(got.total, 16384000 * 1024);
        assert_eq!(parse_meminfo("MemTotal: 1 kB\n"), None);
    }

    #[test]
    fn free_gb_is_the_share_of_the_total() {
        assert!((m(16, 50).free_gb() - 8.0).abs() < 1e-9);
    }

    #[test]
    fn limits_default_to_one_worker_per_gb_and_take_overrides() {
        let mem = m(16, 50);
        assert_eq!(Limits::from(Some(&mem), None, None).max_workers, 16);
        assert_eq!(
            Limits::from(Some(&mem), Some("4"), Some("2.5")).max_workers,
            4
        );
        assert_eq!(
            Limits::from(Some(&mem), Some("x"), Some("2.5")).max_workers,
            16
        );
        assert_eq!(Limits::from(Some(&mem), None, Some("2.5")).min_free_gb, 2.5);
        assert_eq!(Limits::from(None, None, None).max_workers, usize::MAX);
    }

    #[test]
    fn room_stops_at_the_worker_limit_and_at_the_free_floor() {
        let l = Limits::from(None, Some("3"), Some("1"));
        assert!(room(None, &l, 2, 0).is_ok());
        assert!(room(None, &l, 3, 0).unwrap_err().contains("limit of 3"));
        // 8 GB free: plenty.
        assert!(room(Some(&m(16, 50)), &l, 1, 0).is_ok());
        // 1.6 GB free: one more would leave less than the 1 GB floor.
        assert!(room(Some(&m(16, 10)), &l, 1, 0).is_err());
    }

    #[test]
    fn workers_still_starting_count_against_what_is_free() {
        let l = Limits::from(None, Some("50"), Some("1"));
        let mem = m(16, 25); // 4 GB free
        assert!(room(Some(&mem), &l, 0, 0).is_ok());
        assert!(room(Some(&mem), &l, 0, 3).is_ok()); // 4 - 2.25 - 0.75 = 1.0
        assert!(room(Some(&mem), &l, 0, 4).is_err());
    }
}
