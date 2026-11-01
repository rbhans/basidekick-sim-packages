//! Schedule object properties (clause 12.24): reading and writing the
//! weekly schedule, exceptions, default and effective period.
//!
//! Values are BACnetBinaryPV (ENUMERATED inactive/active) or NULL, the way a
//! binary occupancy schedule holds them. Writes are checked as a controller
//! would: wrong types are invalid-data-type, impossible dates and times
//! value-out-of-range. Exceptions take Date, DateRange and WeekNDay entries;
//! there are no Calendar objects to refer to, so references are refused.

use crate::apdu::BacError;
use crate::encoding::*;
use crate::properties::prop::*;
use bask_points::{
    BacObject, CalendarEntry, DatePattern, DateRange, SpecialEvent, TimeValue, WeekNDay,
};

/// Time-value pairs one day (or one exception) may hold, and exceptions per
/// schedule: enough for any real schedule, small enough that a day always
/// fits in a reply.
const MAX_TIME_VALUES: usize = 16;
const MAX_EXCEPTIONS: usize = 32;

/// Non-array schedule properties. False for anything else.
pub fn read(o: &BacObject, property: u32, buf: &mut Vec<u8>) -> bool {
    let Some(s) = o.as_schedule() else {
        return false;
    };
    match property {
        EFFECTIVE_PERIOD => {
            app_date(buf, range_end(s.effective_period.start));
            app_date(buf, range_end(s.effective_period.end));
        }
        SCHEDULE_DEFAULT => app_enumerated(buf, s.default as u32),
        LIST_OF_OBJECT_PROPERTY_REFERENCES => {} // an empty list
        PRIORITY_FOR_WRITING => app_unsigned(buf, 16),
        _ => return false,
    }
    true
}

pub fn array_len(o: &BacObject, property: u32) -> Option<usize> {
    let s = o.as_schedule()?;
    match property {
        WEEKLY_SCHEDULE => Some(7),
        EXCEPTION_SCHEDULE => Some(s.exceptions.len()),
        _ => None,
    }
}

/// One element (0-based) of Weekly_Schedule or Exception_Schedule.
pub fn element(o: &BacObject, property: u32, i: usize, buf: &mut Vec<u8>) -> bool {
    let Some(s) = o.as_schedule() else {
        return false;
    };
    match property {
        WEEKLY_SCHEDULE => daily(buf, &s.weekly[i]),
        EXCEPTION_SCHEDULE => special_event(buf, &s.exceptions[i]),
        _ => return false,
    }
    true
}

/// BACnetDailySchedule.
fn daily(buf: &mut Vec<u8>, day: &[TimeValue]) {
    open(buf, 0);
    time_values(buf, day);
    close(buf, 0);
}

fn time_values(buf: &mut Vec<u8>, values: &[TimeValue]) {
    for tv in values {
        app_time(buf, time_octets(tv.time));
        match tv.value {
            Some(v) => app_enumerated(buf, v as u32),
            None => app_null(buf),
        }
    }
}

/// BACnetSpecialEvent.
fn special_event(buf: &mut Vec<u8>, e: &SpecialEvent) {
    open(buf, 0);
    match e.period {
        CalendarEntry::Date(p) => ctx_octets(buf, 0, &[p.year, p.month, p.day, p.weekday]),
        CalendarEntry::Range(r) => {
            open(buf, 1);
            app_date(buf, range_end(r.start));
            app_date(buf, range_end(r.end));
            close(buf, 1);
        }
        CalendarEntry::WeekNDay(w) => ctx_octets(buf, 2, &[w.month, w.week, w.weekday]),
    }
    close(buf, 0);
    open(buf, 2);
    time_values(buf, &e.values);
    close(buf, 2);
    ctx_unsigned(buf, 3, e.priority as u64);
}

fn range_end(d: Option<chrono::NaiveDate>) -> [u8; 4] {
    d.map_or(ANY_DATE, date_octets)
}

/// Applies a write to one of the writable schedule properties and describes
/// it. `raw` is the encoded value.
pub fn write(
    o: &mut BacObject,
    property: u32,
    index: Option<u64>,
    raw: &[u8],
) -> Result<String, BacError> {
    let s = o.as_schedule_mut().ok_or(BacError::WRITE_ACCESS_DENIED)?;
    let mut r = Reader::new(raw);
    let text = match (property, index) {
        (WEEKLY_SCHEDULE, None) => {
            let mut week = Vec::with_capacity(7);
            while !r.at_end() {
                week.push(decode_daily(&mut r)?);
            }
            s.weekly = week.try_into().map_err(|_| BacError::VALUE_OUT_OF_RANGE)?;
            "weekly-schedule = 7 days".to_string()
        }
        // An array of seven: its size can't change.
        (WEEKLY_SCHEDULE, Some(0)) => return Err(BacError::WRITE_ACCESS_DENIED),
        (WEEKLY_SCHEDULE, Some(i @ 1..=7)) => {
            let day = decode_daily(&mut r)?;
            let text = format!("weekly-schedule[{i}] = {}", describe(&day));
            s.weekly[i as usize - 1] = day;
            text
        }
        (EXCEPTION_SCHEDULE, None) => {
            let mut events = Vec::new();
            while !r.at_end() {
                events.push(decode_special_event(&mut r)?);
            }
            if events.len() > MAX_EXCEPTIONS {
                return Err(BacError::NO_SPACE_TO_WRITE);
            }
            s.exceptions = events;
            format!("exception-schedule = {} entries", s.exceptions.len())
        }
        (EXCEPTION_SCHEDULE, Some(0)) => {
            let n = unsigned(&mut r)? as usize;
            if n > MAX_EXCEPTIONS {
                return Err(BacError::NO_SPACE_TO_WRITE);
            }
            // New entries match no day until they're written.
            s.exceptions.resize(n, filler());
            format!("exception-schedule size = {n}")
        }
        (EXCEPTION_SCHEDULE, Some(i)) if i as usize <= s.exceptions.len() => {
            s.exceptions[i as usize - 1] = decode_special_event(&mut r)?;
            format!("exception-schedule[{i}] written")
        }
        (WEEKLY_SCHEDULE | EXCEPTION_SCHEDULE, Some(_)) => {
            return Err(BacError::INVALID_ARRAY_INDEX);
        }
        (SCHEDULE_DEFAULT, None) => {
            s.default = binary_pv(&mut r)?.ok_or(BacError::INVALID_DATA_TYPE)?;
            format!("schedule-default = {}", on_off(Some(s.default)))
        }
        (EFFECTIVE_PERIOD, None) => {
            let range = decode_range(&mut r)?;
            s.effective_period = range;
            format!(
                "effective-period = {} to {}",
                range.start.map_or("any".into(), |d| d.to_string()),
                range.end.map_or("any".into(), |d| d.to_string())
            )
        }
        _ => return Err(BacError::WRITE_ACCESS_DENIED),
    };
    if !r.at_end() {
        return Err(BacError::INVALID_DATA_TYPE);
    }
    Ok(text)
}

/// Whether a write to `property` changes the schedule's configuration, which
/// bumps the device's database revision.
pub fn is_configuration(property: u32) -> bool {
    matches!(
        property,
        WEEKLY_SCHEDULE | EXCEPTION_SCHEDULE | SCHEDULE_DEFAULT | EFFECTIVE_PERIOD
    )
}

fn filler() -> SpecialEvent {
    SpecialEvent {
        period: CalendarEntry::Date(DatePattern {
            year: 0,
            month: 1,
            day: 1,
            weekday: 255,
        }),
        values: Vec::new(),
        priority: 16,
    }
}

/// Malformed content inside a well-formed write is the wrong data type.
fn bad(_: DecodeError) -> BacError {
    BacError::INVALID_DATA_TYPE
}

fn app(r: &mut Reader) -> Result<Value, BacError> {
    r.read_app().map_err(bad)
}

fn unsigned(r: &mut Reader) -> Result<u64, BacError> {
    match app(r)? {
        Value::Unsigned(n) => Ok(n),
        _ => Err(BacError::INVALID_DATA_TYPE),
    }
}

/// A BACnetBinaryPV, or None for NULL.
fn binary_pv(r: &mut Reader) -> Result<Option<bool>, BacError> {
    match app(r)? {
        Value::Enumerated(0) => Ok(Some(false)),
        Value::Enumerated(1) => Ok(Some(true)),
        Value::Enumerated(_) => Err(BacError::VALUE_OUT_OF_RANGE),
        Value::Null => Ok(None),
        _ => Err(BacError::INVALID_DATA_TYPE),
    }
}

fn decode_time_values(r: &mut Reader, close_tag: u8) -> Result<Vec<TimeValue>, BacError> {
    let mut values = Vec::new();
    while !r.peek_close(close_tag) {
        let Value::Time(t) = app(r)? else {
            return Err(BacError::INVALID_DATA_TYPE);
        };
        let time = time_of(t).ok_or(BacError::VALUE_OUT_OF_RANGE)?;
        values.push(TimeValue {
            time,
            value: binary_pv(r)?,
        });
        if values.len() > MAX_TIME_VALUES {
            return Err(BacError::NO_SPACE_TO_WRITE);
        }
    }
    Ok(values)
}

fn decode_daily(r: &mut Reader) -> Result<Vec<TimeValue>, BacError> {
    r.expect_open(0).map_err(bad)?;
    let day = decode_time_values(r, 0)?;
    r.expect_close(0).map_err(bad)?;
    Ok(day)
}

fn decode_special_event(r: &mut Reader) -> Result<SpecialEvent, BacError> {
    if r.peek_context(1) {
        // calendarReference: no Calendar objects here.
        return Err(BacError::VALUE_OUT_OF_RANGE);
    }
    r.expect_open(0).map_err(bad)?;
    let period = match r.peek_tag() {
        Some(Tag::Context { number: 0, .. }) => {
            let d: [u8; 4] = r
                .ctx_raw(0)
                .map_err(bad)?
                .try_into()
                .map_err(|_| BacError::INVALID_DATA_TYPE)?;
            CalendarEntry::Date(date_pattern(d)?)
        }
        Some(Tag::Open(1)) => {
            r.expect_open(1).map_err(bad)?;
            let range = decode_range(r)?;
            r.expect_close(1).map_err(bad)?;
            CalendarEntry::Range(range)
        }
        Some(Tag::Context { number: 2, .. }) => {
            let [month, week, weekday]: [u8; 3] = r
                .ctx_raw(2)
                .map_err(bad)?
                .try_into()
                .map_err(|_| BacError::INVALID_DATA_TYPE)?;
            let ok = |v: u8, max: u8| v == 255 || (1..=max).contains(&v);
            if !ok(month, 14) || !ok(week, 9) || !ok(weekday, 7) {
                return Err(BacError::VALUE_OUT_OF_RANGE);
            }
            CalendarEntry::WeekNDay(WeekNDay {
                month,
                week,
                weekday,
            })
        }
        _ => return Err(BacError::INVALID_DATA_TYPE),
    };
    r.expect_close(0).map_err(bad)?;
    r.expect_open(2).map_err(bad)?;
    let values = decode_time_values(r, 2)?;
    r.expect_close(2).map_err(bad)?;
    let priority = r.ctx_unsigned(3).map_err(bad)?;
    if !(1..=16).contains(&priority) {
        return Err(BacError::VALUE_OUT_OF_RANGE);
    }
    Ok(SpecialEvent {
        period,
        values,
        priority: priority as u8,
    })
}

fn date_pattern(d: [u8; 4]) -> Result<DatePattern, BacError> {
    let [year, month, day, weekday] = d;
    let ok = |v: u8, max: u8| v == 255 || (1..=max).contains(&v);
    if !ok(month, 14) || !ok(day, 34) || !ok(weekday, 7) {
        return Err(BacError::VALUE_OUT_OF_RANGE);
    }
    Ok(DatePattern {
        year,
        month,
        day,
        weekday,
    })
}

/// BACnetDateRange: two dates, each a real date or entirely unspecified.
fn decode_range(r: &mut Reader) -> Result<DateRange, BacError> {
    let mut end = || -> Result<Option<chrono::NaiveDate>, BacError> {
        let Value::Date(d) = app(r)? else {
            return Err(BacError::INVALID_DATA_TYPE);
        };
        if d[..3] == [255; 3] {
            return Ok(None);
        }
        date_of(d).map(Some).ok_or(BacError::VALUE_OUT_OF_RANGE)
    };
    let range = DateRange {
        start: end()?,
        end: end()?,
    };
    if let (Some(s), Some(e)) = (range.start, range.end)
        && s > e
    {
        return Err(BacError::VALUE_OUT_OF_RANGE);
    }
    Ok(range)
}

fn on_off(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "on",
        Some(false) => "off",
        None => "null",
    }
}

/// `07:00 on, 17:00 off`
fn describe(day: &[TimeValue]) -> String {
    if day.is_empty() {
        return "none".into();
    }
    day.iter()
        .map(|tv| format!("{} {}", tv.time.format("%H:%M"), on_off(tv.value)))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveTime};

    fn sched() -> BacObject {
        BacObject::schedule(1, "S", Default::default())
    }

    fn day(entries: &[([u8; 4], Option<u32>)]) -> Vec<u8> {
        let mut b = Vec::new();
        open(&mut b, 0);
        for (t, v) in entries {
            app_time(&mut b, *t);
            match v {
                Some(v) => app_enumerated(&mut b, *v),
                None => app_null(&mut b),
            }
        }
        close(&mut b, 0);
        b
    }

    #[test]
    fn weekly_writes_whole_and_by_day() {
        let mut o = sched();
        let mon = day(&[([7, 0, 0, 0], Some(1)), ([17, 0, 0, 0], Some(0))]);
        assert_eq!(
            write(&mut o, WEEKLY_SCHEDULE, Some(1), &mon),
            Ok("weekly-schedule[1] = 07:00 on, 17:00 off".into())
        );
        let s = o.as_schedule().unwrap();
        assert_eq!(s.weekly[0].len(), 2);
        // Reads back exactly as written.
        let mut back = Vec::new();
        assert!(element(&o, WEEKLY_SCHEDULE, 0, &mut back));
        assert_eq!(back, mon);

        let week: Vec<u8> = (0..7).flat_map(|_| day(&[([8, 30, 0, 0], None)])).collect();
        assert!(write(&mut o, WEEKLY_SCHEDULE, None, &week).is_ok());
        assert_eq!(o.as_schedule().unwrap().weekly[6][0].value, None);
        // Six days, a bad value, a wrong type, a bad index.
        assert_eq!(
            write(&mut o, WEEKLY_SCHEDULE, None, &week[..week.len() / 7 * 6]),
            Err(BacError::VALUE_OUT_OF_RANGE)
        );
        assert_eq!(
            write(
                &mut o,
                WEEKLY_SCHEDULE,
                Some(2),
                &day(&[([7, 0, 0, 0], Some(2))])
            ),
            Err(BacError::VALUE_OUT_OF_RANGE)
        );
        let mut real = Vec::new();
        open(&mut real, 0);
        app_time(&mut real, [7, 0, 0, 0]);
        app_real(&mut real, 1.0);
        close(&mut real, 0);
        assert_eq!(
            write(&mut o, WEEKLY_SCHEDULE, Some(2), &real),
            Err(BacError::INVALID_DATA_TYPE)
        );
        assert_eq!(
            write(&mut o, WEEKLY_SCHEDULE, Some(8), &mon),
            Err(BacError::INVALID_ARRAY_INDEX)
        );
        assert_eq!(
            write(&mut o, WEEKLY_SCHEDULE, Some(0), &[0x21, 7]),
            Err(BacError::WRITE_ACCESS_DENIED)
        );
    }

    #[test]
    fn exceptions_round_trip_and_refuse_calendars() {
        let mut o = sched();
        let mut b = Vec::new();
        // A holiday by date, then a date range, then the 3rd Friday.
        open(&mut b, 0);
        ctx_octets(&mut b, 0, &[126, 12, 25, 255]);
        close(&mut b, 0);
        open(&mut b, 2);
        app_time(&mut b, [0, 0, 0, 0]);
        app_enumerated(&mut b, 0);
        close(&mut b, 2);
        ctx_unsigned(&mut b, 3, 10);
        open(&mut b, 0);
        open(&mut b, 1);
        // Range ends read back with their weekday filled in.
        app_date(&mut b, [126, 12, 24, 4]);
        app_date(&mut b, [127, 1, 2, 6]);
        close(&mut b, 1);
        close(&mut b, 0);
        open(&mut b, 2);
        close(&mut b, 2);
        ctx_unsigned(&mut b, 3, 16);
        open(&mut b, 0);
        ctx_octets(&mut b, 2, &[255, 3, 5]);
        close(&mut b, 0);
        open(&mut b, 2);
        app_time(&mut b, [12, 0, 0, 0]);
        app_null(&mut b);
        close(&mut b, 2);
        ctx_unsigned(&mut b, 3, 1);
        assert_eq!(
            write(&mut o, EXCEPTION_SCHEDULE, None, &b),
            Ok("exception-schedule = 3 entries".into())
        );
        let s = o.as_schedule().unwrap();
        assert_eq!(
            s.exceptions[1].period,
            CalendarEntry::Range(DateRange {
                start: NaiveDate::from_ymd_opt(2026, 12, 24),
                end: NaiveDate::from_ymd_opt(2027, 1, 2),
            })
        );
        let mut back = Vec::new();
        (0..3).for_each(|i| assert!(element(&o, EXCEPTION_SCHEDULE, i, &mut back)));
        assert_eq!(back, b);

        // Index writes resize and replace.
        assert!(write(&mut o, EXCEPTION_SCHEDULE, Some(0), &[0x21, 1]).is_ok());
        assert_eq!(o.as_schedule().unwrap().exceptions.len(), 1);
        assert_eq!(
            write(&mut o, EXCEPTION_SCHEDULE, Some(2), &b),
            Err(BacError::INVALID_ARRAY_INDEX)
        );

        // A calendar reference, a priority of 0, a backwards range.
        let mut cal = Vec::new();
        ctx_object_id(&mut cal, 1, ObjectId::new(6, 1));
        open(&mut cal, 2);
        close(&mut cal, 2);
        ctx_unsigned(&mut cal, 3, 5);
        assert_eq!(
            write(&mut o, EXCEPTION_SCHEDULE, Some(1), &cal),
            Err(BacError::VALUE_OUT_OF_RANGE)
        );
        let mut zero = b[..b.iter().position(|&x| x == 0x39).unwrap()].to_vec();
        ctx_unsigned(&mut zero, 3, 0);
        assert_eq!(
            write(&mut o, EXCEPTION_SCHEDULE, Some(1), &zero),
            Err(BacError::VALUE_OUT_OF_RANGE)
        );
        let mut range = Vec::new();
        app_date(&mut range, [127, 1, 2, 255]);
        app_date(&mut range, [126, 1, 2, 255]);
        assert_eq!(
            write(&mut o, EFFECTIVE_PERIOD, None, &range),
            Err(BacError::VALUE_OUT_OF_RANGE)
        );
    }

    #[test]
    fn default_and_effective_period() {
        let mut o = sched();
        assert!(write(&mut o, SCHEDULE_DEFAULT, None, &[0x91, 1]).is_ok());
        assert!(o.as_schedule().unwrap().default);
        assert_eq!(
            write(&mut o, SCHEDULE_DEFAULT, None, &[0x00]),
            Err(BacError::INVALID_DATA_TYPE)
        );
        let mut range = Vec::new();
        app_date(&mut range, [126, 1, 1, 255]);
        app_date(&mut range, ANY_DATE);
        assert_eq!(
            write(&mut o, EFFECTIVE_PERIOD, None, &range),
            Ok("effective-period = 2026-01-01 to any".into())
        );
        let mut back = Vec::new();
        assert!(read(&o, EFFECTIVE_PERIOD, &mut back));
        assert_eq!(back, [0xA4, 126, 1, 1, 4, 0xA4, 255, 255, 255, 255]);
        let t = NaiveDate::from_ymd_opt(2025, 6, 1)
            .unwrap()
            .and_time(NaiveTime::MIN);
        assert!(o.as_schedule().unwrap().value_at(t));
        assert_eq!(
            write(&mut o, PRIORITY_FOR_WRITING, None, &[0x21, 8]),
            Err(BacError::WRITE_ACCESS_DENIED)
        );
    }
}
