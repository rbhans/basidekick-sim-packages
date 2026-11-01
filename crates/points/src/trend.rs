//! Trend logs: a ring buffer of samples of one object's present value in the
//! same device, polled on interval boundaries of sim time (clause 12.25).
//!
//! Samples land on multiples of Log_Interval counted from midnight, so a
//! 15-minute log reads :00, :15, :30, :45. A step longer than the interval
//! takes one sample at the latest boundary rather than making some up; a
//! leap of the clock (see [`PointDb::tick`]) leaves a log-interrupted status
//! record and the log carries on from the next boundary.
//!
//! [`PointDb::tick`]: crate::PointDb::tick

use crate::{ObjectType, PointValue};
use chrono::{DateTime, Duration, NaiveDateTime};
use std::collections::VecDeque;

/// BACnetLogStatus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LogStatus {
    pub disabled: bool,
    pub purged: bool,
    pub interrupted: bool,
}

/// What one record holds (the logDatum choices the simulator uses).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LogDatum {
    Status(LogStatus),
    Boolean(bool),
    Real(f32),
    /// Binary values, as BACnetBinaryPV.
    Enumerated(u32),
    /// Multi-state values.
    Unsigned(u32),
}

impl LogDatum {
    /// A sampled present value in the logged object's own terms.
    pub fn of(value: PointValue) -> Self {
        match value {
            PointValue::Real(x) => LogDatum::Real(x),
            PointValue::Binary(b) => LogDatum::Enumerated(b as u32),
            PointValue::Multi(m) => LogDatum::Unsigned(m),
        }
    }

    /// The number a sample stands for; None for status records.
    pub fn as_f64(self) -> Option<f64> {
        match self {
            LogDatum::Status(_) => None,
            LogDatum::Boolean(b) => Some(b as u8 as f64),
            LogDatum::Real(x) => Some(x as f64),
            LogDatum::Enumerated(n) | LogDatum::Unsigned(n) => Some(n as f64),
        }
    }
}

/// BACnetLogRecord, with the sequence number ReadRange counts by.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LogRecord {
    pub seq: u32,
    pub time: NaiveDateTime,
    pub datum: LogDatum,
    /// The logged object's status flags; None on status records.
    pub flags: Option<[bool; 4]>,
}

#[derive(Clone, Debug)]
pub struct TrendLog {
    /// The logged object (its present value), in the same device.
    pub target: (ObjectType, u32),
    pub enable: bool,
    /// Log_Interval, hundredths of a second; at least 100.
    pub interval: u32,
    pub buffer_size: u32,
    records: VecDeque<LogRecord>,
    /// Total_Record_Count: every record ever added, and the last one's
    /// sequence number. Wraps to 1.
    total: u32,
    next_due: Option<NaiveDateTime>,
}

impl TrendLog {
    pub fn new(target: (ObjectType, u32), interval_s: u32, buffer_size: u32) -> Self {
        let buffer_size = buffer_size.max(1);
        TrendLog {
            target,
            enable: true,
            interval: interval_s.max(1).saturating_mul(100),
            buffer_size,
            records: VecDeque::with_capacity(buffer_size.min(4096) as usize),
            total: 0,
            next_due: None,
        }
    }

    /// The samples, oldest first, as (time, value). Status records are left out.
    pub fn records(&self) -> impl Iterator<Item = (NaiveDateTime, f64)> + '_ {
        self.records
            .iter()
            .filter_map(|r| Some((r.time, r.datum.as_f64()?)))
    }

    /// Every record in the buffer, oldest first, status records included.
    pub fn buffer(&self) -> &VecDeque<LogRecord> {
        &self.records
    }

    pub fn record_count(&self) -> u32 {
        self.records.len() as u32
    }

    pub fn total_record_count(&self) -> u32 {
        self.total
    }

    /// Enable on or off, recorded with a status record when it changes.
    pub fn set_enable(&mut self, enable: bool, now: NaiveDateTime) {
        if enable != self.enable {
            self.enable = enable;
            self.next_due = None;
            self.push(
                now,
                LogDatum::Status(LogStatus {
                    disabled: !enable,
                    ..Default::default()
                }),
                None,
            );
        }
    }

    /// A new Log_Interval in hundredths; the next sample lands on its grid.
    pub fn set_interval(&mut self, hundredths: u32) {
        self.interval = hundredths.max(100);
        self.next_due = None;
    }

    /// Empties the buffer (Record_Count written 0), leaving a buffer-purged
    /// status record.
    pub fn clear(&mut self, now: NaiveDateTime) {
        self.records.clear();
        self.push(
            now,
            LogDatum::Status(LogStatus {
                purged: true,
                ..Default::default()
            }),
            None,
        );
    }

    /// Whether a sample is due at `now`, and the boundary to stamp it with.
    /// `leap` says the clock jumped since the last tick.
    pub(crate) fn poll(&mut self, now: NaiveDateTime, leap: bool) -> Option<NaiveDateTime> {
        if !self.enable {
            return None;
        }
        let ms = self.interval as i64 * 10;
        // Only a log that was already running notes the gap.
        if leap && self.next_due.take().is_some() {
            self.push(
                now,
                LogDatum::Status(LogStatus {
                    interrupted: true,
                    ..Default::default()
                }),
                None,
            );
        }
        let due = *self.next_due.get_or_insert_with(|| boundary(now, ms, true));
        if now < due {
            return None;
        }
        let at = boundary(now, ms, false);
        self.next_due = Some(at + Duration::milliseconds(ms));
        Some(at)
    }

    pub(crate) fn push(&mut self, time: NaiveDateTime, datum: LogDatum, flags: Option<[bool; 4]>) {
        self.total = self.total.checked_add(1).unwrap_or(1);
        while self.records.len() >= self.buffer_size as usize {
            self.records.pop_front();
        }
        self.records.push_back(LogRecord {
            seq: self.total,
            time,
            datum,
            flags,
        });
    }
}

/// The interval boundary at or after `t` (`up`) or at or before it.
fn boundary(t: NaiveDateTime, ms: i64, up: bool) -> NaiveDateTime {
    let now = t.and_utc().timestamp_millis();
    let mut b = now.div_euclid(ms) * ms;
    if up && b < now {
        b += ms;
    }
    DateTime::from_timestamp_millis(b).map_or(t, |d| d.naive_utc())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn t(h: u32, m: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 2)
            .unwrap()
            .and_hms_opt(h, m, s)
            .unwrap()
    }

    #[test]
    fn samples_land_on_boundaries() {
        let mut log = TrendLog::new((ObjectType::AnalogInput, 1), 60, 100);
        // Ticks every 0.7 s from 08:00:29.5: the first sample is at 08:01:00.
        let mut now = t(8, 0, 29) + Duration::milliseconds(500);
        let mut got = Vec::new();
        while now < t(8, 3, 1) {
            if let Some(at) = log.poll(now, false) {
                got.push(at);
                log.push(at, LogDatum::Real(1.0), Some([false; 4]));
            }
            now += Duration::milliseconds(700);
        }
        assert_eq!(got, [t(8, 1, 0), t(8, 2, 0), t(8, 3, 0)]);
        assert_eq!(log.record_count(), 3);

        // Coarse steps (a 5 s jump-ahead step on a 1 s log) sample once per
        // step, at the latest boundary.
        let mut log = TrendLog::new((ObjectType::AnalogInput, 1), 1, 100);
        assert_eq!(log.poll(t(9, 0, 0), false), Some(t(9, 0, 0)));
        assert_eq!(
            log.poll(t(9, 0, 5) + Duration::milliseconds(300), false),
            Some(t(9, 0, 5))
        );
        assert_eq!(
            log.poll(t(9, 0, 5) + Duration::milliseconds(600), false),
            None
        );
    }

    #[test]
    fn leaps_and_status_records() {
        let mut log = TrendLog::new((ObjectType::AnalogInput, 1), 900, 5);
        assert_eq!(log.poll(t(8, 0, 0), false), Some(t(8, 0, 0)));
        log.push(t(8, 0, 0), LogDatum::Real(70.0), Some([false; 4]));
        // Ten hours in one go: one interrupted record, no back-fill, and
        // logging picks up at the next boundary.
        assert_eq!(log.poll(t(18, 7, 0), true), None);
        assert_eq!(log.record_count(), 2);
        assert!(matches!(
            log.buffer()[1].datum,
            LogDatum::Status(LogStatus {
                interrupted: true,
                ..
            })
        ));
        assert_eq!(log.poll(t(18, 15, 0), false), Some(t(18, 15, 0)));

        log.set_enable(false, t(18, 20, 0));
        assert_eq!(log.poll(t(18, 30, 0), false), None);
        log.set_enable(true, t(18, 31, 0));
        assert_eq!(log.poll(t(18, 45, 0), false), Some(t(18, 45, 0)));
        // A ring of 5: the oldest falls off, sequence numbers keep counting.
        log.push(t(18, 45, 0), LogDatum::Real(71.0), Some([false; 4]));
        log.push(t(19, 0, 0), LogDatum::Real(72.0), Some([false; 4]));
        assert_eq!(log.record_count(), 5);
        assert_eq!(log.total_record_count(), 6);
        assert_eq!(log.buffer()[0].seq, 2);
        assert_eq!(
            log.records().collect::<Vec<_>>(),
            [(t(18, 45, 0), 71.0), (t(19, 0, 0), 72.0)]
        );

        log.clear(t(19, 1, 0));
        assert_eq!(log.record_count(), 1);
        assert_eq!(log.buffer()[0].seq, 7);
        assert!(log.records().next().is_none());
    }
}
