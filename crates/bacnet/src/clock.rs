//! Local wall-clock time for the device object's local-date and local-time.
//!
//! A BAS reads these to check a controller's clock. The simulated devices
//! report the simulated building's time once the engine runs it, and the
//! host's before that; the UTC offset and daylight saving are always the
//! host's. TimeSynchronization is ignored: the sim owns the clock.

use chrono::{Datelike, NaiveDateTime, Timelike};
#[cfg(not(windows))]
use std::time::{SystemTime, UNIX_EPOCH};

pub struct LocalTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    /// 1 = Monday … 7 = Sunday, as BACnet dates count.
    pub weekday: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub hundredths: u8,
    /// Minutes from local standard time to UTC: positive west of Greenwich,
    /// which is BACnet's sign for utc-offset.
    pub utc_offset: i32,
    pub dst: bool,
}

impl LocalTime {
    /// BACnet Date octets.
    pub fn date(&self) -> [u8; 4] {
        [
            (self.year - 1900).clamp(0, 254) as u8,
            self.month,
            self.day,
            self.weekday,
        ]
    }

    /// BACnet Time octets.
    pub fn time(&self) -> [u8; 4] {
        [self.hour, self.minute, self.second, self.hundredths]
    }
}

/// The sim's time `sim` if it has one, with the host's offset and DST.
pub fn device_time(sim: Option<NaiveDateTime>) -> LocalTime {
    let host = now();
    let Some(t) = sim else {
        return host;
    };
    LocalTime {
        year: t.year(),
        month: t.month() as u8,
        day: t.day() as u8,
        weekday: t.weekday().number_from_monday() as u8,
        hour: t.hour() as u8,
        minute: t.minute() as u8,
        second: t.second().min(59) as u8,
        hundredths: (t.nanosecond() / 10_000_000).min(99) as u8,
        ..host
    }
}

#[cfg(unix)]
pub fn now() -> LocalTime {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = since.as_secs() as libc::time_t;
    // SAFETY: localtime_r only writes the tm we hand it; all-zero is a valid tm.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return utc(since.as_secs(), since.subsec_millis());
    }
    let dst = tm.tm_isdst > 0;
    let standard_east = tm.tm_gmtoff as i32 - if dst { 3600 } else { 0 };
    LocalTime {
        year: tm.tm_year + 1900,
        month: tm.tm_mon as u8 + 1,
        day: tm.tm_mday as u8,
        weekday: if tm.tm_wday == 0 { 7 } else { tm.tm_wday as u8 },
        hour: tm.tm_hour as u8,
        minute: tm.tm_min as u8,
        second: tm.tm_sec.min(59) as u8,
        hundredths: (since.subsec_millis() / 10) as u8,
        utc_offset: -standard_east / 60,
        dst,
    }
}

#[cfg(windows)]
pub fn now() -> LocalTime {
    use chrono::{Local, Offset, TimeZone};
    let t = Local::now();
    let east = |month| {
        Local
            .with_ymd_and_hms(t.year(), month, 15, 12, 0, 0)
            .single()
            .map(|d| d.offset().fix().local_minus_utc())
            .unwrap_or(0)
    };
    // standard time is the smaller of the winter and summer offsets, in
    // either hemisphere
    let now_east = t.offset().fix().local_minus_utc();
    let standard_east = east(1).min(east(7));
    LocalTime {
        year: t.year(),
        month: t.month() as u8,
        day: t.day() as u8,
        weekday: t.weekday().number_from_monday() as u8,
        hour: t.hour() as u8,
        minute: t.minute() as u8,
        second: t.second().min(59) as u8,
        hundredths: (t.timestamp_subsec_millis() / 10) as u8,
        utc_offset: -standard_east / 60,
        dst: now_east > standard_east,
    }
}

/// Without a time zone database, report UTC.
#[cfg(not(any(unix, windows)))]
pub fn now() -> LocalTime {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    utc(since.as_secs(), since.subsec_millis())
}

#[cfg_attr(windows, allow(dead_code))]
fn utc(secs: u64, millis: u32) -> LocalTime {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (yoe + era * 400 + (month <= 2) as i64) as i32;
    LocalTime {
        year,
        month,
        day,
        // 1970-01-01 was a Thursday.
        weekday: ((days + 3).rem_euclid(7) + 1) as u8,
        hour: (rem / 3600) as u8,
        minute: (rem / 60 % 60) as u8,
        second: (rem % 60) as u8,
        hundredths: (millis / 10) as u8,
        utc_offset: 0,
        dst: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_civil_dates() {
        let t = utc(0, 0);
        assert_eq!((t.year, t.month, t.day, t.weekday), (1970, 1, 1, 4));
        // 2026-10-30 13:45:07 UTC, a Friday
        let t = utc(1_793_367_907, 500);
        assert_eq!((t.year, t.month, t.day, t.weekday), (2026, 10, 30, 5));
        assert_eq!(t.time(), [13, 45, 7, 50]);
        assert_eq!(t.date(), [126, 10, 30, 5]);
        // 2024-02-29, a Thursday
        let t = utc(1_709_164_800, 0);
        assert_eq!((t.year, t.month, t.day, t.weekday), (2024, 2, 29, 4));
    }

    #[test]
    fn local_time_is_sane() {
        let t = now();
        assert!(
            (1..=12).contains(&t.month)
                && (1..=31).contains(&t.day)
                && (1..=7).contains(&t.weekday)
        );
        assert!(t.utc_offset.abs() <= 14 * 60);
    }
}
