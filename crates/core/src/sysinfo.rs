//! A snapshot of the machine conditions that move scan times.
//!
//! Two runs of the same scan on the same library have differed by six
//! seconds with nothing in the code to explain it, and the log recorded
//! nothing about the machine to explain it either. This module records what
//! is cheap to read and known to matter, once per scan.
//!
//! What it deliberately does **not** claim to answer is "was the file cache
//! warm". Windows has no API for "is this other file's data resident" - the
//! residency calls (`QueryWorkingSetEx`, `VirtualQuery`) only speak about
//! pages *this* process has mapped, and the system-wide cache size below
//! says how much is cached, never what. The honest warm/cold signal for a
//! scan is the throughput the `$MFT` read actually achieved, which
//! `perf::report` prints beside its duration: hundreds of MB/s means the
//! platter, several GB/s means memory.

use std::fmt;

/// What the log records about the machine at the start of a scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemState {
    /// Installed physical memory, bytes.
    pub ram_total: u64,
    /// Physical memory not in use, bytes. A scan that starts with little of
    /// it has less room for the volume's `$MFT` to stay cached.
    pub ram_available: u64,
    /// Bytes currently held by the system file cache.
    pub file_cache: u64,
    /// Logical processors - the denominator for every per-worker figure in
    /// `perf::report`.
    pub logical_cpus: usize,
    /// `Some(true)` on mains, `Some(false)` on battery, `None` when the
    /// machine does not say. A laptop on battery clocks down, and the same
    /// scan then takes a different time for no reason visible in the code.
    pub on_ac_power: Option<bool>,
}

impl fmt::Display for SystemState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let gb = |bytes: u64| bytes as f64 / 1024.0 / 1024.0 / 1024.0;
        write!(
            f,
            "RAM {:.1} GB ({:.1} GB free), file cache {:.1} GB, {} logical CPUs, power {}",
            gb(self.ram_total),
            gb(self.ram_available),
            gb(self.file_cache),
            self.logical_cpus,
            match self.on_ac_power {
                Some(true) => "AC",
                Some(false) => "battery",
                None => "unknown",
            }
        )
    }
}

fn logical_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(0)
}

#[cfg(windows)]
pub fn system_state() -> SystemState {
    use windows::Win32::System::Power::GetSystemPowerStatus;
    use windows::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut memory = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: `memory` is a correctly sized, correctly stamped MEMORYSTATUSEX
    // owned by this frame. A failure leaves it at its zeroed default, which
    // prints as 0.0 GB rather than a wrong number.
    let _ = unsafe { GlobalMemoryStatusEx(&mut memory) };

    let mut perf = PERFORMANCE_INFORMATION::default();
    let perf_size = std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32;
    // SAFETY: same - a stack-owned struct and its true size; the cache field
    // stays zero if the call fails.
    let cache_pages = unsafe {
        if GetPerformanceInfo(&mut perf, perf_size).is_ok() {
            perf.SystemCache
        } else {
            0
        }
    };
    let page_size = if perf.PageSize == 0 {
        4096
    } else {
        perf.PageSize
    };

    let mut power = Default::default();
    // SAFETY: stack-owned SYSTEM_POWER_STATUS; only read when the call
    // reports success.
    let queried_power = unsafe { GetSystemPowerStatus(&mut power) }.is_ok();
    // 0 offline, 1 online, 255 unknown - anything else is not an answer.
    let on_ac_power = match (queried_power, power.ACLineStatus) {
        (true, 0) => Some(false),
        (true, 1) => Some(true),
        _ => None,
    };

    SystemState {
        ram_total: memory.ullTotalPhys,
        ram_available: memory.ullAvailPhys,
        file_cache: (cache_pages * page_size) as u64,
        logical_cpus: logical_cpus(),
        on_ac_power,
    }
}

/// Non-Windows builds exist only to keep the crate compiling for the
/// portability check - see `portability_regression_tests` - and have no
/// scanning to describe.
#[cfg(not(windows))]
pub fn system_state() -> SystemState {
    SystemState {
        ram_total: 0,
        ram_available: 0,
        file_cache: 0,
        logical_cpus: logical_cpus(),
        on_ac_power: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of the line is to be readable in a log next to a scan time,
    /// so what is worth pinning is that every field reaches it and that a
    /// real machine reports plausible memory rather than zeroes.
    #[test]
    fn the_line_names_every_field_it_measured() {
        let state = system_state();
        let line = state.to_string();

        for expected in ["RAM", "free", "file cache", "logical CPUs", "power"] {
            assert!(
                line.contains(expected),
                "{expected:?} missing from {line:?}"
            );
        }
        assert!(
            state.logical_cpus > 0,
            "a running test has at least one CPU"
        );

        #[cfg(windows)]
        {
            assert!(state.ram_total > 0, "Windows must report installed memory");
            assert!(
                state.ram_available <= state.ram_total,
                "free memory ({}) exceeds installed ({})",
                state.ram_available,
                state.ram_total
            );
        }
    }
}
