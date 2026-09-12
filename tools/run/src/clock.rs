//! Owns the UTC calendar date and timestamp that name and stamp a run record.
//!
//! A record path and a journal line state UTC, never host local time, so one campaign is
//! named the same way on every host that runs it. This module owns no duration and no
//! budget.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};

/// The last second this module converts: 9999-12-31T23:59:59Z.
///
/// Every later conversion is bounded by this value, so no intermediate product leaves the
/// range of `i64`.
const MAX_SECONDS: u64 = 253_402_300_799;

const SECONDS_PER_DAY: u64 = 86_400;
const SECONDS_PER_HOUR: u64 = 3_600;
const SECONDS_PER_MINUTE: u64 = 60;

/// A civil date and time in UTC.
pub struct Utc {
    year: i64,
    month: i64,
    day: i64,
    hour: u64,
    minute: u64,
    second: u64,
}

impl Utc {
    /// Reads the host clock.
    ///
    /// # Errors
    ///
    /// Fails when the host clock reports a time before 1970 or after 9999.
    pub fn now() -> Result<Self> {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::tool("clock", "reports a time before 1970"))?
            .as_secs();
        Self::from_unix_seconds(seconds)
    }

    /// The date a record directory is named with.
    pub fn date(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// The instant a manifest or a journal line is stamped with.
    pub fn timestamp(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    // Every operand below is bounded by MAX_SECONDS, which the guard rejects above, so no
    // sum, product, or difference here can overflow.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "operands bounded by MAX_SECONDS"
    )]
    fn from_unix_seconds(seconds: u64) -> Result<Self> {
        if seconds > MAX_SECONDS {
            return Err(Error::tool("clock", "reports a time after 9999"));
        }
        let days = i64::try_from(seconds / SECONDS_PER_DAY)
            .map_err(|_| Error::tool("clock", "reports a day outside the supported range"))?;
        let rest = seconds % SECONDS_PER_DAY;
        let (year, month, day) = civil_from_days(days);
        Ok(Self {
            year,
            month,
            day,
            hour: rest / SECONDS_PER_HOUR,
            minute: (rest % SECONDS_PER_HOUR) / SECONDS_PER_MINUTE,
            second: rest % SECONDS_PER_MINUTE,
        })
    }
}

/// Converts days since 1970-01-01 into a civil year, month, and day.
///
/// This is Howard Hinnant's `civil_from_days`, which is exact for the proleptic Gregorian
/// calendar and needs no timezone database.
// `days` is at most 2_932_896, so the largest product below is under 4e6.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "days bounded by MAX_SECONDS"
)]
const fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::Utc;

    fn stamp(seconds: u64) -> String {
        Utc::from_unix_seconds(seconds)
            .map_or_else(|_| String::from("error"), |utc| utc.timestamp())
    }

    #[test]
    fn epoch_is_the_first_of_january_1970() {
        assert_eq!(stamp(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn a_day_boundary_rolls_the_date() {
        assert_eq!(stamp(86_399), "1970-01-01T23:59:59Z");
        assert_eq!(stamp(86_400), "1970-01-02T00:00:00Z");
    }

    #[test]
    fn a_leap_day_is_a_date_of_its_own() {
        assert_eq!(stamp(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn known_instants_convert_exactly() {
        assert_eq!(stamp(1_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(stamp(2_147_483_647), "2038-01-19T03:14:07Z");
    }

    #[test]
    fn the_date_drops_the_time() {
        let date = Utc::from_unix_seconds(1_000_000_000).map(|utc| utc.date());
        assert_eq!(date.ok().as_deref(), Some("2001-09-09"));
    }

    #[test]
    fn a_time_beyond_the_supported_range_is_rejected() {
        assert!(Utc::from_unix_seconds(253_402_300_800).is_err());
    }
}
