//! Owns the environment a result records: the host, the toolchain, the build, the thread
//! count, the frequency policy, and the counter backend.
//!
//! A benchmark number means nothing without the machine it was produced on. Everything here
//! is read from the host at run time or from the build that produced this binary, never
//! assumed, and a fact the host does not expose is recorded as unavailable with the reason
//! rather than filled in.
//!
//! This module does not own what is measured or what a measurement licenses.

use std::path::Path;

use crate::counters::{self, Backend};
use crate::exec::{self, argv};

/// The host and the build a result was produced on.
pub struct Environment {
    pub os: &'static str,
    pub os_version: String,
    pub arch: &'static str,
    pub cpu_model: String,
    /// The CPU features this binary was compiled to use.
    ///
    /// The harness dispatches on none, so what it was compiled for is what it ran.
    pub cpu_features: String,
    pub cores: String,
    pub memory: String,
    /// Whether the frequency policy of the host is readable, and what it is when it is.
    pub frequency_policy: String,
    pub rustc: String,
    pub cargo: String,
    pub build_profile: &'static str,
    pub counter: Backend,
    /// The revision of the repository the harness was built from.
    pub revision: String,
    pub harness_version: &'static str,
}

/// Reads everything the host will state about itself.
pub fn capture() -> Environment {
    Environment {
        os: std::env::consts::OS,
        os_version: os_version(),
        arch: std::env::consts::ARCH,
        cpu_model: cpu_model(),
        cpu_features: compiled_features(),
        cores: cores(),
        memory: memory(),
        frequency_policy: frequency_policy(),
        rustc: tool_line("rustc", &["--version"]),
        cargo: tool_line("cargo", &["--version"]),
        build_profile: if cfg!(debug_assertions) {
            "debug, with debug assertions on"
        } else {
            "release"
        },
        counter: counters::detect(),
        revision: revision(),
        harness_version: env!("CARGO_PKG_VERSION"),
    }
}

fn revision() -> String {
    let commit = tool_line("git", &["rev-parse", "HEAD"]);
    // A dirty tree is recorded rather than hidden: it is a different input set.
    if query("git", &["status", "--porcelain"]).is_some_and(|text| !text.is_empty()) {
        format!("{commit} (dirty)")
    } else {
        commit
    }
}

fn os_version() -> String {
    query("uname", &["-sr"]).unwrap_or_else(|| String::from("unavailable: `uname` is absent"))
}

fn cpu_model() -> String {
    if cfg!(target_os = "macos")
        && let Some(brand) = query("sysctl", &["-n", "machdep.cpu.brand_string"])
    {
        return brand;
    }
    if let Ok(text) = std::fs::read_to_string("/proc/cpuinfo")
        && let Some(model) = text
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(key, _)| key.contains("model name"))
            })
            .map(|(_, value)| String::from(value.trim()))
    {
        return model;
    }
    String::from("unavailable: this host publishes no CPU model")
}

fn cores() -> String {
    std::thread::available_parallelism()
        .map_or_else(|_| String::from("unavailable"), |count| count.to_string())
}

fn memory() -> String {
    if cfg!(target_os = "macos")
        && let Some(bytes) = query("sysctl", &["-n", "hw.memsize"])
    {
        return format!("{bytes} bytes");
    }
    if let Ok(text) = std::fs::read_to_string("/proc/meminfo")
        && let Some(line) = text.lines().find(|line| line.starts_with("MemTotal"))
    {
        return String::from(line.trim());
    }
    String::from("unavailable: this host publishes no memory total")
}

/// The frequency policy, which decides whether two samples ran at the same clock.
///
/// Linux publishes a governor per core. A host that publishes none says so, because an
/// unstated policy is not a stable one.
fn frequency_policy() -> String {
    let governor = Path::new("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor");
    if let Ok(text) = std::fs::read_to_string(governor) {
        return format!("scaling_governor: {}", text.trim());
    }
    String::from(
        "unavailable: this host publishes no governor, boost, or thermal state that a run \
         can read or control. A sample taken under an unstated frequency policy is what the \
         spread between samples reports.",
    )
}

/// The CPU features this binary was compiled to use.
///
/// The harness runs no dispatch of its own, so the compiled set is the executed set. It is a
/// property of the build, not a probe of the host, and it is labelled as such.
fn compiled_features() -> String {
    let mut features: Vec<&str> = Vec::new();
    for (name, present) in [
        ("neon", cfg!(target_feature = "neon")),
        ("sse2", cfg!(target_feature = "sse2")),
        ("sse4.2", cfg!(target_feature = "sse4.2")),
        ("avx", cfg!(target_feature = "avx")),
        ("avx2", cfg!(target_feature = "avx2")),
        ("crc", cfg!(target_feature = "crc")),
        ("aes", cfg!(target_feature = "aes")),
    ] {
        if present {
            features.push(name);
        }
    }
    if features.is_empty() {
        String::from("the target's baseline only")
    } else {
        features.join(", ")
    }
}

fn tool_line(program: &str, args: &[&str]) -> String {
    query(program, args).unwrap_or_else(|| format!("unavailable: `{program}` is absent"))
}

fn query(program: &str, args: &[&str]) -> Option<String> {
    let mut command = vec![String::from(program)];
    command.extend(argv(args));
    exec::capture(&command, Path::new(".")).ok()
}

/// The instant a record or a result is stamped with, in UTC.
///
/// `date` is a required host tool. A record that cannot state when it was produced is not a
/// record, so a host without it fails here rather than stamping something invented.
///
/// # Errors
///
/// Fails when the host has no `date`.
pub fn timestamp() -> crate::error::Result<String> {
    exec::capture(&argv(["date", "-u", "+%Y-%m-%dT%H:%M:%SZ"]), Path::new("."))
}

/// The resident-set high-water mark of this process, in bytes.
///
/// It is process wide and monotonic: it bounds what a measurement held, and it does not
/// attribute it. The codec-owned figure is the attributable one.
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
pub const PEAK_RSS_METHOD: &str = "getrusage(RUSAGE_SELF).ru_maxrss, which is process wide \
and monotonic. It bounds what the measurement held, including the harness input and output \
buffers, and it attributes nothing to the codec. The codec-owned figure is the attributable \
one.";

/// The thread count a measurement ran with, stated beside every number.
///
/// Every measurement in this harness is single threaded except the parallel scaling metric,
/// which states its own count.
pub const MEASUREMENT_THREADS: u32 = 1;

#[cfg(test)]
mod tests {
    use super::{MEASUREMENT_THREADS, capture, compiled_features};

    #[test]
    fn a_capture_states_every_fact_or_why_it_cannot() {
        let environment = capture();
        for field in [
            environment.os_version.as_str(),
            environment.cpu_model.as_str(),
            environment.cores.as_str(),
            environment.memory.as_str(),
            environment.frequency_policy.as_str(),
            environment.rustc.as_str(),
            environment.cargo.as_str(),
            environment.revision.as_str(),
        ] {
            assert!(!field.trim().is_empty());
        }
    }

    #[test]
    fn a_capture_names_its_counter_backend_whatever_the_host_answers() {
        let environment = capture();
        assert_eq!(environment.counter.name, crate::counters::BACKEND);
    }

    #[test]
    fn the_compiled_feature_set_is_never_blank() {
        assert!(!compiled_features().is_empty());
    }

    #[test]
    fn a_measurement_states_one_thread_unless_it_says_otherwise() {
        assert_eq!(MEASUREMENT_THREADS, 1);
    }

    #[test]
    fn a_timestamp_is_a_utc_instant() {
        let stamp = super::timestamp().unwrap_or_default();
        assert!(stamp.ends_with('Z'), "{stamp}");
        assert!(stamp.len() >= 20, "{stamp}");
    }

    #[test]
    fn the_host_reports_a_resident_set_high_water_mark() {
        let peak = super::peak_rss_bytes();
        assert!(peak.is_none_or(|bytes| bytes > 0));
    }

    #[test]
    fn a_test_build_reports_that_it_is_not_a_release_build() {
        assert!(capture().build_profile.contains("debug"));
    }
}
