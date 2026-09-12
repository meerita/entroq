//! Owns the host facts a proof states beside its numbers.
//!
//! This module does not own what a number means. It reports what the operating system will
//! say, or that the operating system refused to say it.

/// The resident-set high-water mark of this process, in bytes.
///
/// It is process wide and monotonic: it bounds what the process held at its worst moment, and
/// it attributes nothing. A measurement that needs the figure for one interval runs that
/// interval in its own process.
///
/// Returns `None` when the host refuses the call.
// The only interface that reports a process's resident-set high-water mark is a C system
// call, and there is no safe way to reach one.
#[allow(unsafe_code)]
pub fn peak_rss_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: the call writes a complete `rusage` through the pointer, or returns non-zero
    // and writes nothing.
    let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if status != 0 {
        return None;
    }
    // SAFETY: the call above returned success, so it initialized the value.
    let usage = unsafe { usage.assume_init() };
    let reported = u64::try_from(usage.ru_maxrss).ok()?;
    // Darwin reports bytes and Linux reports kibibytes, which the two kernels document.
    Some(if cfg!(target_os = "macos") {
        reported
    } else {
        reported.saturating_mul(1024)
    })
}

/// How the resident-set figure was obtained, stated beside it.
pub const PEAK_RSS_METHOD: &str = "getrusage(RUSAGE_SELF).ru_maxrss, read in the child process \
that performed the run, so the figure covers that run and no other";

/// The architecture, operating system, and byte order this binary was built for.
///
/// The byte order is the fact the cross-architecture proof turns on. The other two name the
/// lane that produced a result.
pub fn platform() -> String {
    format!(
        "{}-{}, {}-endian",
        std::env::consts::ARCH,
        std::env::consts::OS,
        if cfg!(target_endian = "little") {
            "little"
        } else {
            "big"
        }
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_host_reports_a_resident_set_high_water_mark() {
        // A host that refuses the call is a fact a proof states, not a test failure.
        if let Some(peak) = super::peak_rss_bytes() {
            assert!(peak > 0);
        }
    }

    #[test]
    fn the_binary_states_the_platform_it_was_built_for() {
        let platform = super::platform();
        assert!(platform.contains("endian"), "{platform}");
        assert!(platform.contains(std::env::consts::ARCH), "{platform}");
    }
}
