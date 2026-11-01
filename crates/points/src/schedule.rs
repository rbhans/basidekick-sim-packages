//! Schedule objects: a week of on/off times, exceptions on top of it, and a
//! default for when nothing applies (clause 12.24).
//!
//! Evaluation follows the standard. Each day starts fresh at midnight with
//! Schedule_Default; a time-value pair holds until the next one that day; the
//! highest-priority exception in effect beats the weekly schedule. A NULL
//! value relinquishes, letting the next lower priority (or the default) show
//! through. Outside Effective_Period the default applies all day.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Weekday};

/// One entry of a day's schedule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeValue {
    pub time: NaiveTime,
    /// None is a BACnet NULL: relinquish.
    pub value: Option<bool>,
}

/// A BACnet Date, which may hold wildcards (255) and the special months
/// (13 odd, 14 even) and days (32 last, 33 odd, 34 even).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DatePattern {
    /// Years since 1900.
    pub year: u8,
    pub month: u8,
    pub day: u8,
    /// 1 = Monday … 7 = Sunday.
    pub weekday: u8,
}

impl DatePattern {
    pub const ANY: DatePattern = DatePattern {
        year: 255,
        month: 255,
        day: 255,
        weekday: 255,
    };

    pub fn of(d: NaiveDate) -> Self {
        DatePattern {
            year: (d.year() - 1900).clamp(0, 254) as u8,
            month: d.month() as u8,
            day: d.day() as u8,
            weekday: d.weekday().number_from_monday() as u8,
        }
    }

    pub fn matches(&self, d: NaiveDate) -> bool {
        let last = last_day(d);
        (self.year == 255 || d.year() - 1900 == self.year as i32)
            && month_matches(self.month, d.month())
            && match self.day {
                255 => true,
                32 => d.day() == last,
                33 => d.day() % 2 == 1,
                34 => d.day().is_multiple_of(2),
                n => d.day() == n as u32,
            }
            && (self.weekday == 255 || d.weekday().number_from_monday() == self.weekday as u32)
    }

    /// The date this names, when year, month and day are all given.
    pub fn to_date(&self) -> Option<NaiveDate> {
        if self.year == 255 || self.month > 12 || self.day > 31 {
            return None;
        }
        NaiveDate::from_ymd_opt(1900 + self.year as i32, self.month as u32, self.day as u32)
    }
}

/// BACnetWeekNDay: a month, a week of the month and a weekday, 255 for any.
/// Weeks 1–5 are days 1–7, 8–14 … 29–31; week 6 is the last seven days of the
/// month, and 7–9 the seven days before that, and so on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WeekNDay {
    pub month: u8,
    pub week: u8,
    pub weekday: u8,
}

impl WeekNDay {
    pub fn matches(&self, d: NaiveDate) -> bool {
        let (day, last) = (d.day(), last_day(d));
        let week = match self.week {
            255 => true,
            1..=5 => (day - 1) / 7 + 1 == self.week as u32,
            // 6: the last 7 days; 7: the 7 before those; and so on.
            6..=9 => {
                let from_end = last - day; // 0 on the last day
                from_end / 7 == self.week as u32 - 6
            }
            _ => false,
        };
        month_matches(self.month, d.month())
            && week
            && (self.weekday == 255 || d.weekday().number_from_monday() == self.weekday as u32)
    }
}

/// An inclusive span of dates; None at either end leaves it open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DateRange {
    pub start: Option<NaiveDate>,
    pub end: Option<NaiveDate>,
}

impl DateRange {
    pub fn contains(&self, d: NaiveDate) -> bool {
        self.start.is_none_or(|s| s <= d) && self.end.is_none_or(|e| d <= e)
    }
}

/// When an exception applies. Calendar-object references aren't supported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalendarEntry {
    Date(DatePattern),
    Range(DateRange),
    WeekNDay(WeekNDay),
}

impl CalendarEntry {
    pub fn matches(&self, d: NaiveDate) -> bool {
        match self {
            CalendarEntry::Date(p) => p.matches(d),
            CalendarEntry::Range(r) => r.contains(d),
            CalendarEntry::WeekNDay(w) => w.matches(d),
        }
    }
}

/// One entry of Exception_Schedule (BACnetSpecialEvent).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpecialEvent {
    pub period: CalendarEntry,
    pub values: Vec<TimeValue>,
    /// 1 (highest) … 16.
    pub priority: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schedule {
    /// Monday first.
    pub weekly: [Vec<TimeValue>; 7],
    pub exceptions: Vec<SpecialEvent>,
    pub default: bool,
    pub effective_period: DateRange,
}

impl Schedule {
    /// A schedule from the engine's weekly on/off times (Monday first), with
    /// no exceptions, an inactive default and no effective-period limits.
    pub fn new(weekly: [Vec<(NaiveTime, bool)>; 7]) -> Self {
        Schedule {
            weekly: weekly.map(|day| {
                day.into_iter()
                    .map(|(time, v)| TimeValue {
                        time,
                        value: Some(v),
                    })
                    .collect()
            }),
            exceptions: Vec::new(),
            default: false,
            effective_period: DateRange::default(),
        }
    }

    /// One day of the weekly schedule, as written.
    pub fn day(&self, weekday: Weekday) -> &[TimeValue] {
        &self.weekly[weekday.num_days_from_monday() as usize]
    }

    /// The schedule's value at `t`, exceptions and effective period included.
    pub fn value_at(&self, t: NaiveDateTime) -> bool {
        let (date, time) = (t.date(), t.time());
        if !self.effective_period.contains(date) {
            return self.default;
        }
        // The highest priority in effect; the earlier entry wins a tie.
        let mut best: Option<(u8, bool)> = None;
        for e in &self.exceptions {
            if best.is_some_and(|(p, _)| p <= e.priority) || !e.period.matches(date) {
                continue;
            }
            if let Some(v) = in_effect(&e.values, time) {
                best = Some((e.priority, v));
            }
        }
        match best {
            Some((_, v)) => v,
            None => in_effect(self.day(date.weekday()), time).unwrap_or(self.default),
        }
    }

    /// The first time after `t`, up to `t + within`, that the value changes,
    /// and what it changes to.
    pub fn next_change(&self, t: NaiveDateTime, within: Duration) -> Option<(NaiveDateTime, bool)> {
        let end = t.checked_add_signed(within).unwrap_or(NaiveDateTime::MAX);
        let current = self.value_at(t);
        let mut day = t.date();
        // The value can only change at midnight or at a listed time, so
        // those are the only instants worth checking.
        while day.and_time(NaiveTime::MIN) <= end {
            let mut times: Vec<NaiveTime> = std::iter::once(NaiveTime::MIN)
                .chain(self.day(day.weekday()).iter().map(|tv| tv.time))
                .chain(
                    self.exceptions
                        .iter()
                        .filter(|e| e.period.matches(day))
                        .flat_map(|e| e.values.iter().map(|tv| tv.time)),
                )
                .collect();
            times.sort_unstable();
            times.dedup();
            for at in times.into_iter().map(|time| day.and_time(time)) {
                if at <= t || at > end {
                    continue;
                }
                let v = self.value_at(at);
                if v != current {
                    return Some((at, v));
                }
            }
            day = day.succ_opt()?;
        }
        None
    }
}

/// What a day's list holds at `time`: its latest entry at or before then (the
/// later one of a tie). None before the first entry or when that's a NULL.
fn in_effect(values: &[TimeValue], time: NaiveTime) -> Option<bool> {
    values
        .iter()
        .filter(|tv| tv.time <= time)
        .max_by_key(|tv| tv.time)?
        .value
}

fn month_matches(pattern: u8, month: u32) -> bool {
    match pattern {
        255 => true,
        13 => month % 2 == 1,
        14 => month.is_multiple_of(2),
        m => month == m as u32,
    }
}

fn last_day(d: NaiveDate) -> u32 {
    let (y, m) = (d.year(), d.month());
    let next = if m == 12 {
        NaiveDate::from_ymd_opt(y + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(y, m + 1, 1)
    };
    next.and_then(|n| n.pred_opt()).map_or(31, |l| l.day())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hm(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn at(y: i32, mo: u32, d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_time(hm(h, m))
    }

    /// Weekdays 7:00–17:00, weekends off.
    fn office() -> Schedule {
        let day = vec![(hm(7, 0), true), (hm(17, 0), false)];
        Schedule::new([
            day.clone(),
            day.clone(),
            day.clone(),
            day.clone(),
            day,
            vec![],
            vec![],
        ])
    }

    #[test]
    fn weekly_schedule_and_default() {
        let s = office();
        // 2026-11-02 is a Monday.
        assert!(!s.value_at(at(2026, 11, 2, 6, 59)));
        assert!(s.value_at(at(2026, 11, 2, 7, 0)));
        assert!(s.value_at(at(2026, 11, 2, 16, 59)));
        assert!(!s.value_at(at(2026, 11, 2, 17, 0)));
        assert!(!s.value_at(at(2026, 11, 7, 12, 0))); // Saturday
        assert_eq!(s.day(Weekday::Mon).len(), 2);
        assert!(s.day(Weekday::Sun).is_empty());

        // Friday evening → Monday morning, across two midnights.
        assert_eq!(
            s.next_change(at(2026, 11, 6, 18, 0), Duration::days(4)),
            Some((at(2026, 11, 9, 7, 0), true))
        );
        assert_eq!(
            s.next_change(at(2026, 11, 6, 18, 0), Duration::days(2)),
            None
        );
        assert_eq!(
            s.next_change(at(2026, 11, 2, 7, 0), Duration::hours(24)),
            Some((at(2026, 11, 2, 17, 0), false))
        );
    }

    #[test]
    fn each_day_starts_fresh_at_midnight() {
        // On at 22:00 Monday; Tuesday has nothing, so it's back to the
        // default at midnight. Wednesday carries on with an entry at 00:00.
        let mut s = Schedule::new([
            vec![(hm(22, 0), true)],
            vec![],
            vec![(hm(0, 0), true), (hm(6, 0), false)],
            vec![],
            vec![],
            vec![],
            vec![],
        ]);
        assert!(s.value_at(at(2026, 11, 2, 23, 59)));
        assert!(!s.value_at(at(2026, 11, 3, 0, 0)));
        assert_eq!(
            s.next_change(at(2026, 11, 2, 23, 0), Duration::hours(4)),
            Some((at(2026, 11, 3, 0, 0), false))
        );
        assert!(s.value_at(at(2026, 11, 4, 0, 0)));
        assert!(!s.value_at(at(2026, 11, 4, 6, 0)));
        // A NULL relinquishes to the default.
        s.default = true;
        s.weekly[0].push(TimeValue {
            time: hm(23, 0),
            value: None,
        });
        s.weekly[0].push(TimeValue {
            time: hm(22, 30),
            value: Some(false),
        });
        assert!(!s.value_at(at(2026, 11, 2, 22, 45)));
        assert!(s.value_at(at(2026, 11, 2, 23, 30)));
        assert!(s.value_at(at(2026, 11, 3, 12, 0)));
    }

    #[test]
    fn exceptions_by_priority() {
        let mut s = office();
        // A holiday on a Monday, off all day.
        s.exceptions.push(SpecialEvent {
            period: CalendarEntry::Date(DatePattern::of(
                NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
            )),
            values: vec![TimeValue {
                time: hm(0, 0),
                value: Some(false),
            }],
            priority: 10,
        });
        assert!(!s.value_at(at(2026, 11, 2, 9, 0)));
        assert!(s.value_at(at(2026, 11, 3, 9, 0)));
        // An after-hours event that week at a higher priority wins from
        // 18:00 and gives way at 21:00 (NULL), back to the holiday.
        s.exceptions.push(SpecialEvent {
            period: CalendarEntry::Range(DateRange {
                start: NaiveDate::from_ymd_opt(2026, 11, 1),
                end: NaiveDate::from_ymd_opt(2026, 11, 7),
            }),
            values: vec![
                TimeValue {
                    time: hm(18, 0),
                    value: Some(true),
                },
                TimeValue {
                    time: hm(21, 0),
                    value: None,
                },
            ],
            priority: 5,
        });
        assert!(!s.value_at(at(2026, 11, 2, 17, 59)));
        assert!(s.value_at(at(2026, 11, 2, 18, 0)));
        assert!(!s.value_at(at(2026, 11, 2, 21, 0)));
        assert!(s.value_at(at(2026, 11, 3, 19, 0)));
        assert!(!s.value_at(at(2026, 11, 9, 19, 0)));
        assert_eq!(
            s.next_change(at(2026, 11, 2, 12, 0), Duration::days(1)),
            Some((at(2026, 11, 2, 18, 0), true))
        );
        assert_eq!(
            s.next_change(at(2026, 11, 2, 18, 0), Duration::days(1)),
            Some((at(2026, 11, 2, 21, 0), false))
        );
        // Third Thursday of any month, all day.
        s.exceptions.push(SpecialEvent {
            period: CalendarEntry::WeekNDay(WeekNDay {
                month: 255,
                week: 3,
                weekday: 4,
            }),
            values: vec![TimeValue {
                time: hm(0, 0),
                value: Some(true),
            }],
            priority: 16,
        });
        assert!(s.value_at(at(2026, 11, 19, 3, 0)));
        assert!(!s.value_at(at(2026, 11, 12, 3, 0)));

        // Outside the effective period: the default, exceptions and all.
        s.effective_period.end = NaiveDate::from_ymd_opt(2026, 11, 15);
        assert!(!s.value_at(at(2026, 11, 19, 3, 0)));
        assert!(!s.value_at(at(2026, 11, 16, 9, 0)));
        assert!(s.value_at(at(2026, 11, 13, 9, 0)));
    }

    #[test]
    fn date_patterns() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        let any_last = DatePattern {
            day: 32,
            ..DatePattern::ANY
        };
        assert!(any_last.matches(d(2028, 2, 29)) && !any_last.matches(d(2028, 2, 28)));
        let odd_months = DatePattern {
            month: 13,
            day: 1,
            ..DatePattern::ANY
        };
        assert!(odd_months.matches(d(2026, 3, 1)) && !odd_months.matches(d(2026, 4, 1)));
        let fridays = DatePattern {
            weekday: 5,
            ..DatePattern::ANY
        };
        assert!(fridays.matches(d(2026, 11, 6)) && !fridays.matches(d(2026, 11, 5)));
        assert_eq!(
            DatePattern::of(d(2026, 11, 6)).to_date(),
            Some(d(2026, 11, 6))
        );
        assert_eq!(fridays.to_date(), None);
        // Last Monday of May: Memorial Day.
        let memorial = WeekNDay {
            month: 5,
            week: 6,
            weekday: 1,
        };
        assert!(memorial.matches(d(2026, 5, 25)) && !memorial.matches(d(2026, 5, 18)));
        // 5th week is days 29–31
        let fifth = WeekNDay {
            month: 255,
            week: 5,
            weekday: 255,
        };
        assert!(fifth.matches(d(2026, 1, 29)) && !fifth.matches(d(2026, 1, 28)));
    }
}
