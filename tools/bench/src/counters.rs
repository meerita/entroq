//! Owns the hardware counter backend: what it is, and whether the host grants it.
//!
//! A cycle count and an instruction count come from a performance monitor unit the process
//! is permitted to read. Permission is a property of the host, not of the codec, so this
//! module reports what the host allows and nothing else.
//!
//! It never derives a cycle count from elapsed time and a nominal frequency. A host that
//! scales frequency, or that mixes core types, turns that product into a number with no
//! meaning, and a number with no meaning is worse in a result than an absent one.
//!
//! One backend exists: the `perf_event_open` system call. It is the only interface reachable
//! from an unprivileged process that reports a real count. On a host that does not offer it,
//! or that denies it, the backend reports unavailable with the reason it was given.
//!
//! This module does not own what a metric means or where it is written.

/// Whether the host grants the counter, and why when it does not.
///
/// A host that grants it constructs the first variant. No host this project has does, and
/// the variant is the backend's contract rather than this host's answer to it.
#[allow(dead_code)]
pub enum Availability {
    Granted,
    Denied(String),
}

impl Availability {
    pub const fn is_granted(&self) -> bool {
        matches!(self, Self::Granted)
    }

    pub const fn reason(&self) -> Option<&str> {
        match self {
            Self::Granted => None,
            Self::Denied(reason) => Some(reason.as_str()),
        }
    }
}

/// The counter backend a run used, and what it reported.
pub struct Backend {
    pub name: &'static str,
    pub availability: Availability,
}

/// The name every run states, whether or not the host grants it.
pub const BACKEND: &str = "perf_event_open";

/// Asks the host for the counter, and reports what it answered.
pub fn detect() -> Backend {
    Backend {
        name: BACKEND,
        availability: probe(),
    }
}

/// The reason a metric that needs the counter carries when the host denies it.
pub fn unavailable_reason(backend: &Backend) -> String {
    backend
        .availability
        .reason()
        .map_or_else(String::new, |reason| {
            format!(
                "the {} backend is not available on this host: {reason}. No number is derived \
             from elapsed time and a nominal frequency, because this CPU scales frequency \
             and mixes core types, which makes that product meaningless.",
                backend.name
            )
        })
}

#[cfg(target_os = "linux")]
fn probe() -> Availability {
    linux::probe()
}

#[cfg(not(target_os = "linux"))]
fn probe() -> Availability {
    Availability::Denied(format!(
        "`perf_event_open` is a Linux system call, and this host runs {}. No unprivileged \
         interface on this host reports a cycle count: the performance monitor registers \
         trap at the user exception level, and the profiling tools that reach them need \
         either privilege or a vendor toolchain, so neither is reproducible harness tooling.",
        std::env::consts::OS
    ))
}

#[cfg(target_os = "linux")]
mod linux {
    //! The Linux backend. It opens one counter, reads whether the kernel allowed it, and
    //! closes it again. The harness asks once per run, before any measurement.

    use super::Availability;

    /// The kernel's `perf_event_attr`, at the size this backend declares.
    ///
    /// The layout is a kernel ABI. The kernel reads `size` to decide which fields exist, so
    /// a struct that matches an older or newer kernel still works.
    #[repr(C)]
    #[derive(Default)]
    struct Attr {
        kind: u32,
        size: u32,
        config: u64,
        sample_period_or_freq: u64,
        sample_type: u64,
        read_format: u64,
        flags: u64,
        wakeup: u32,
        bp_type: u32,
        config1: u64,
        config2: u64,
        branch_sample_type: u64,
        sample_regs_user: u64,
        sample_stack_user: u32,
        clockid: i32,
        sample_regs_intr: u64,
        aux_watermark: u32,
        sample_max_stack: u16,
        reserved_2: u16,
        aux_sample_size: u32,
        reserved_3: u32,
        sig_data: u64,
        config3: u64,
    }

    /// `PERF_TYPE_HARDWARE`.
    const TYPE_HARDWARE: u32 = 0;
    /// `PERF_COUNT_HW_CPU_CYCLES`.
    const COUNT_HW_CPU_CYCLES: u64 = 0;
    /// `disabled | exclude_kernel | exclude_hv`, the flags an unprivileged counter needs.
    const FLAGS: u64 = 1 | (1 << 5) | (1 << 6);

    #[allow(unsafe_code)]
    pub fn probe() -> Availability {
        let mut attr = Attr {
            kind: TYPE_HARDWARE,
            config: COUNT_HW_CPU_CYCLES,
            flags: FLAGS,
            ..Attr::default()
        };
        attr.size = u32::try_from(size_of::<Attr>()).unwrap_or(0);

        // SAFETY: `attr` is a live, correctly sized `perf_event_attr`, and the remaining
        // arguments are the documented constants for "this process, any CPU, no group".
        let descriptor = unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                std::ptr::addr_of!(attr),
                0,
                -1,
                -1,
                0,
            )
        };
        let Ok(descriptor) = i32::try_from(descriptor) else {
            return Availability::Denied(String::from(
                "`perf_event_open` returned a descriptor this backend cannot hold",
            ));
        };
        if descriptor < 0 {
            let error = std::io::Error::last_os_error();
            return Availability::Denied(format!(
                "`perf_event_open` failed: {error}. A shared or virtualized host usually \
                 denies the performance monitor unit, and `perf_event_paranoid` decides \
                 what an unprivileged process may open."
            ));
        }
        // SAFETY: `descriptor` is the open descriptor the call above returned.
        let _ = unsafe { libc::close(descriptor) };
        Availability::Granted
    }
}

#[cfg(test)]
mod tests {
    use super::{Availability, BACKEND, detect, unavailable_reason};

    #[test]
    fn a_run_always_names_one_backend() {
        assert_eq!(detect().name, BACKEND);
    }

    #[test]
    fn a_denied_counter_carries_its_reason() {
        let denied = Availability::Denied(String::from("the host says no"));
        assert!(!denied.is_granted());
        assert_eq!(denied.reason(), Some("the host says no"));
    }

    #[test]
    fn a_granted_counter_carries_no_reason() {
        assert!(Availability::Granted.is_granted());
        assert!(Availability::Granted.reason().is_none());
    }

    #[test]
    fn the_unavailable_reason_refuses_a_derived_number_in_writing() {
        let backend = super::Backend {
            name: BACKEND,
            availability: Availability::Denied(String::from("no permission")),
        };
        let reason = unavailable_reason(&backend);
        assert!(reason.contains("no permission"), "{reason}");
        assert!(reason.contains("nominal frequency"), "{reason}");
    }

    #[test]
    fn a_granted_counter_has_nothing_to_explain() {
        let backend = super::Backend {
            name: BACKEND,
            availability: Availability::Granted,
        };
        assert!(unavailable_reason(&backend).is_empty());
    }

    #[test]
    fn this_host_reports_its_answer_rather_than_guessing() {
        let backend = detect();
        // The development host denies it and a granting host exists in principle. Either
        // answer is a result; an answer that states nothing is not.
        assert!(backend.availability.is_granted() || backend.availability.reason().is_some());
    }
}
