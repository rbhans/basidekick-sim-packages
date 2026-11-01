//! Trend log properties (clause 12.25) and ReadRange (15.8), the only way
//! to read a Log_Buffer.
//!
//! ReadRange selects records by position (1 = oldest in the buffer), by
//! sequence number, or by time, counting forward or (negative count)
//! backward from the reference. The reply has to fit one unsegmented APDU:
//! when it doesn't, it carries the records nearest the reference and sets
//! more-items, and the client asks again from where it left off.

use crate::apdu::{BacError, REJECT_PARAMETER_OUT_OF_RANGE};
use crate::encoding::*;
use crate::properties::{self, Target, prop::*};
use crate::services::{Reply, single_value};
use bask_points::{BacObject, LogDatum, LogRecord, PointDb};
use chrono::NaiveDateTime;

/// Non-array trend log properties. False for anything else.
pub fn read(o: &BacObject, property: u32, buf: &mut Vec<u8>) -> bool {
    let Some(t) = o.as_trend_log() else {
        return false;
    };
    match property {
        ENABLE => app_bool(buf, t.enable),
        STOP_WHEN_FULL => app_bool(buf, false),
        BUFFER_SIZE => app_unsigned(buf, t.buffer_size as u64),
        RECORD_COUNT => app_unsigned(buf, t.record_count() as u64),
        TOTAL_RECORD_COUNT => app_unsigned(buf, t.total_record_count() as u64),
        LOGGING_TYPE => app_enumerated(buf, 0), // polled
        LOG_INTERVAL => app_unsigned(buf, t.interval as u64),
        LOG_DEVICE_OBJECT_PROPERTY => {
            // BACnetDeviceObjectPropertyReference, in this device.
            let (ty, instance) = t.target;
            ctx_object_id(buf, 0, ObjectId::new(ty.bacnet_id(), instance));
            ctx_enumerated(buf, 1, PRESENT_VALUE);
        }
        _ => return false,
    }
    true
}

/// Applies a write to one of the writable trend log properties.
pub fn write(
    o: &mut BacObject,
    property: u32,
    raw: &[u8],
    now: NaiveDateTime,
) -> Result<String, BacError> {
    let t = o.as_trend_log_mut().ok_or(BacError::WRITE_ACCESS_DENIED)?;
    match property {
        ENABLE => {
            let Value::Boolean(b) = single_value(raw)? else {
                return Err(BacError::INVALID_DATA_TYPE);
            };
            t.set_enable(b, now);
            Ok(format!("enable = {b}"))
        }
        LOG_INTERVAL => {
            let Value::Unsigned(n) = single_value(raw)? else {
                return Err(BacError::INVALID_DATA_TYPE);
            };
            // At least a second, in hundredths.
            if !(100..=u32::MAX as u64).contains(&n) {
                return Err(BacError::VALUE_OUT_OF_RANGE);
            }
            t.set_interval(n as u32);
            Ok(format!("log-interval = {} s", n as f64 / 100.0))
        }
        RECORD_COUNT => match single_value(raw)? {
            Value::Unsigned(0) => {
                t.clear(now);
                Ok("record-count = 0 (buffer cleared)".into())
            }
            Value::Unsigned(_) => Err(BacError::VALUE_OUT_OF_RANGE),
            _ => Err(BacError::INVALID_DATA_TYPE),
        },
        _ => Err(BacError::WRITE_ACCESS_DENIED),
    }
}

/// BACnetLogRecord.
fn log_record(buf: &mut Vec<u8>, r: &LogRecord) {
    open(buf, 0);
    app_date_time(buf, Some(r.time));
    close(buf, 0);
    open(buf, 1);
    match r.datum {
        LogDatum::Status(s) => ctx_bit_string(buf, 0, &[s.disabled, s.purged, s.interrupted]),
        LogDatum::Boolean(b) => ctx_bool(buf, 1, b),
        LogDatum::Real(x) => ctx_real(buf, 2, x),
        LogDatum::Enumerated(e) => ctx_enumerated(buf, 3, e),
        LogDatum::Unsigned(u) => ctx_unsigned(buf, 4, u as u64),
    }
    close(buf, 1);
    if let Some(flags) = r.flags {
        ctx_bit_string(buf, 2, &flags);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Range {
    Position(u64, i64),
    Sequence(u64, i64),
    Time(Option<NaiveDateTime>, i64),
}

impl Range {
    fn count(self) -> i64 {
        match self {
            Range::Position(_, c) | Range::Sequence(_, c) | Range::Time(_, c) => c,
        }
    }
}

/// The reference and count inside a range's brackets.
fn range_args(r: &mut Reader) -> DResult<(Value, i64)> {
    let reference = r.read_app()?;
    let count = match r.read_app()? {
        Value::Signed(c) => c,
        Value::Unsigned(c) => i64::try_from(c).map_err(|_| DecodeError::BadValue)?,
        _ => return Err(DecodeError::BadValue),
    };
    Ok((reference, count))
}

pub fn read_range(db: &PointDb, dev: usize, body: &[u8], max_apdu: usize) -> DResult<Reply> {
    let mut r = Reader::new(body);
    let id = r.ctx_object_id(0)?;
    let property = r.ctx_enumerated(1)?;
    let index = r.opt_ctx_unsigned(2)?;
    let range = if r.peek_open(3) {
        r.expect_open(3)?;
        let (Value::Unsigned(i), c) = range_args(&mut r)? else {
            return Err(DecodeError::BadValue);
        };
        r.expect_close(3)?;
        Some(Range::Position(i, c))
    } else if r.peek_open(6) {
        r.expect_open(6)?;
        let (Value::Unsigned(s), c) = range_args(&mut r)? else {
            return Err(DecodeError::BadValue);
        };
        r.expect_close(6)?;
        Some(Range::Sequence(s, c))
    } else if r.peek_open(7) {
        r.expect_open(7)?;
        let Value::Date(date) = r.read_app()? else {
            return Err(DecodeError::BadValue);
        };
        let (Value::Time(time), c) = range_args(&mut r)? else {
            return Err(DecodeError::BadValue);
        };
        r.expect_close(7)?;
        let t = date_of(date).zip(time_of(time)).map(|(d, t)| d.and_time(t));
        Some(Range::Time(t, c))
    } else {
        None
    };
    if !r.at_end() {
        return Err(DecodeError::TooManyArguments);
    }
    if range.is_some_and(|r| r.count() == 0) {
        return Ok(Reply::Reject(REJECT_PARAMETER_OUT_OF_RANGE));
    }

    let d = &db.devices[dev];
    let id = properties::unwildcard(d, id);
    let log = match properties::resolve(d, id) {
        Err(e) => return Ok(Reply::Error(e)),
        Ok(t) if !properties::has_property(d, t, property) => {
            return Ok(Reply::Error(BacError::UNKNOWN_PROPERTY));
        }
        Ok(Target::Object(i)) if property == LOG_BUFFER => d.objects[i].as_trend_log(),
        Ok(_) => None,
    };
    let Some(log) = log else {
        return Ok(Reply::Error(BacError::PROPERTY_IS_NOT_A_LIST));
    };
    if index.is_some() {
        return Ok(Reply::Error(BacError::NOT_AN_ARRAY));
    }

    let records = log.buffer();
    let n = records.len();
    // The matching records as a span of buffer positions, and whether the
    // count runs backward from the reference.
    let (lo, hi, backward) = match range {
        None => (0, n, false),
        Some(Range::Position(i, c)) => {
            if i == 0 || i > n as u64 {
                (0, 0, false)
            } else {
                span(i as usize - 1, c, n)
            }
        }
        Some(Range::Sequence(s, c)) => {
            // Clip the requested sequence numbers to what's still held.
            let first = records.front().map_or(0, |r| r.seq as i64);
            let (from, to) = if c > 0 {
                (s as i64, s as i64 + c - 1)
            } else {
                (s as i64 + c + 1, s as i64)
            };
            let lo = (from - first).clamp(0, n as i64) as usize;
            let hi = (to - first + 1).clamp(0, n as i64) as usize;
            (lo, hi.max(lo), c < 0)
        }
        Some(Range::Time(t, c)) => {
            let Some(t) = t else {
                return Ok(Reply::Error(BacError::VALUE_OUT_OF_RANGE));
            };
            if c > 0 {
                let lo = records.iter().position(|r| r.time > t).unwrap_or(n);
                (lo, (lo + c as usize).min(n), false)
            } else {
                let hi = records
                    .iter()
                    .rposition(|r| r.time < t)
                    .map_or(0, |p| p + 1);
                (hi.saturating_sub(c.unsigned_abs() as usize), hi, true)
            }
        }
    };

    let mut ack = Vec::new();
    ctx_object_id(&mut ack, 0, id);
    ctx_enumerated(&mut ack, 1, property);
    // APDU header, result flags, item count, the item brackets and the
    // first sequence number, at their largest.
    let fixed = 3 + ack.len() + 3 + 5 + 2 + 5;
    let budget = max_apdu.saturating_sub(fixed);
    // Records nearest the reference first, as many as fit.
    let mut items: Vec<Vec<u8>> = Vec::new();
    let mut used = 0;
    let order: Box<dyn Iterator<Item = usize>> = if backward {
        Box::new((lo..hi).rev())
    } else {
        Box::new(lo..hi)
    };
    for p in order {
        let mut item = Vec::new();
        log_record(&mut item, &records[p]);
        if used + item.len() > budget {
            break;
        }
        used += item.len();
        items.push(item);
    }
    let (first, last) = if backward {
        items.reverse();
        (hi - items.len(), hi)
    } else {
        (lo, lo + items.len())
    };
    let count = items.len();
    let flags = [
        count > 0 && first == 0,
        count > 0 && last == n,
        count < hi - lo,
    ];
    ctx_bit_string(&mut ack, 3, &flags);
    ctx_unsigned(&mut ack, 4, count as u64);
    open(&mut ack, 5);
    items.iter().for_each(|i| ack.extend_from_slice(i));
    close(&mut ack, 5);
    if count > 0 && matches!(range, Some(Range::Sequence(..) | Range::Time(..))) {
        ctx_unsigned(&mut ack, 6, records[first].seq as u64);
    }
    Ok(Reply::ComplexAck(ack))
}

/// Positions `count` records from 0-based `reference`: forward including
/// it, or backward ending at it.
fn span(reference: usize, count: i64, n: usize) -> (usize, usize, bool) {
    if count > 0 {
        (reference, (reference + count as usize).min(n), false)
    } else {
        let hi = reference + 1;
        (hi.saturating_sub(count.unsigned_abs() as usize), hi, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bask_points::{BacDevice, ObjectType, PointValue};
    use chrono::{Duration, NaiveDate};

    fn t0() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 2)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
    }

    /// A one-minute log of a ramp, ticked for `minutes`.
    fn db(minutes: i64, buffer: u32) -> PointDb {
        let mut db = PointDb::new(2001);
        db.devices.push(BacDevice {
            instance: 1,
            name: "D".into(),
            description: String::new(),
            location: String::new(),
            model_name: String::new(),
            mac: None,
            online: true,
            objects: vec![
                BacObject::new(ObjectType::AnalogInput, 1, "X", PointValue::Real(0.0)),
                BacObject::trend_log(1, "X-TL", (ObjectType::AnalogInput, 1), 60, buffer),
            ],
            database_revision: 1,
        });
        for m in 0..minutes {
            db.devices[0].objects[0].present_value = PointValue::Real(m as f32);
            db.tick(t0() + Duration::minutes(m));
        }
        db
    }

    fn request(range: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut b = Vec::new();
        ctx_object_id(&mut b, 0, ObjectId::new(20, 1));
        ctx_enumerated(&mut b, 1, LOG_BUFFER);
        range(&mut b);
        b
    }

    /// (flags, item count, the items' values, first sequence number)
    fn parse(reply: Reply) -> ([bool; 3], u64, Vec<f32>, Option<u64>) {
        let Reply::ComplexAck(ack) = reply else {
            panic!("{reply:?}")
        };
        let mut r = Reader::new(&ack);
        r.ctx_object_id(0).unwrap();
        r.ctx_enumerated(1).unwrap();
        let f = r.ctx_raw(3).unwrap();
        let flags = [0, 1, 2].map(|i| f[1] & (0x80 >> i) != 0);
        let count = r.ctx_unsigned(4).unwrap();
        let items = r.bracketed(5).unwrap();
        let mut values = Vec::new();
        let mut ir = Reader::new(items);
        while !ir.at_end() {
            ir.bracketed(0).unwrap();
            let datum = ir.bracketed(1).unwrap();
            values.push(f32::from_be_bytes(datum[1..5].try_into().unwrap()));
            ir.ctx_raw(2).unwrap();
        }
        let first = r.opt_ctx_unsigned(6).unwrap();
        assert!(r.at_end());
        (flags, count, values, first)
    }

    #[test]
    fn by_position_with_negative_counts() {
        let db = db(10, 100);
        let pos = |i, c| {
            request(move |b| {
                open(b, 3);
                app_unsigned(b, i);
                app_signed(b, c);
                close(b, 3);
            })
        };
        let rr = |body: &[u8]| parse(read_range(&db, 0, body, 1476).unwrap());
        assert_eq!(
            rr(&pos(1, 3)),
            ([true, false, false], 3, vec![0.0, 1.0, 2.0], None)
        );
        assert_eq!(
            rr(&pos(10, -2)),
            ([false, true, false], 2, vec![8.0, 9.0], None)
        );
        assert_eq!(rr(&pos(9, 5)).2, vec![8.0, 9.0]);
        assert_eq!(rr(&pos(2, -5)).2, vec![0.0, 1.0]);
        assert_eq!(rr(&pos(11, 1)).1, 0);
        // No range: everything.
        assert_eq!(
            rr(&request(|_| {})),
            (
                [true, true, false],
                10,
                (0..10).map(|v| v as f32).collect(),
                None
            )
        );
        assert_eq!(
            read_range(&db, 0, &pos(1, 0), 1476),
            Ok(Reply::Reject(REJECT_PARAMETER_OUT_OF_RANGE))
        );
    }

    #[test]
    fn by_sequence_and_time() {
        // A ring of 5 after 10 samples: sequence numbers 6–10 remain.
        let db = db(10, 5);
        let seq = |s, c| {
            request(move |b| {
                open(b, 6);
                app_unsigned(b, s);
                app_signed(b, c);
                close(b, 6);
            })
        };
        let rr = |body: &[u8]| parse(read_range(&db, 0, body, 1476).unwrap());
        assert_eq!(
            rr(&seq(7, 2)),
            ([false, false, false], 2, vec![6.0, 7.0], Some(7))
        );
        // Rolled off the front: clipped to what's left.
        assert_eq!(
            rr(&seq(1, 7)),
            ([true, false, false], 2, vec![5.0, 6.0], Some(6))
        );
        assert_eq!(
            rr(&seq(10, -3)),
            ([false, true, false], 3, vec![7.0, 8.0, 9.0], Some(8))
        );
        assert_eq!(rr(&seq(11, 5)), ([false; 3], 0, vec![], None));

        let time = |m: i64, c| {
            request(move |b| {
                open(b, 7);
                app_date_time(b, Some(t0() + Duration::minutes(m)));
                app_signed(b, c);
                close(b, 7);
            })
        };
        // Newer than 00:06: 7, 8. Older than 00:08: 6, 7 (counting back).
        assert_eq!(
            rr(&time(6, 2)),
            ([false, false, false], 2, vec![7.0, 8.0], Some(8))
        );
        assert_eq!(
            rr(&time(8, -2)),
            ([false, false, false], 2, vec![6.0, 7.0], Some(7))
        );
        assert_eq!(rr(&time(0, -2)).1, 0);
    }

    #[test]
    fn truncates_to_fit_and_says_so() {
        let db = db(200, 1000);
        let (flags, count, values, _) = parse(read_range(&db, 0, &request(|_| {}), 480).unwrap());
        assert_eq!(flags, [true, false, true]);
        assert!(count > 10 && count < 30, "{count}");
        assert_eq!(values[0], 0.0);
        // Backward from the newest: the newest ones, in order.
        let back = request(|b| {
            open(b, 3);
            app_unsigned(b, 200);
            app_signed(b, -200);
            close(b, 3);
        });
        let (flags, count, values, _) = parse(read_range(&db, 0, &back, 480).unwrap());
        assert_eq!(flags, [false, true, true]);
        assert_eq!(*values.last().unwrap(), 199.0);
        assert_eq!(values[0], 200.0 - count as f32);

        // Not a log buffer, or not a list at all.
        let mut b = Vec::new();
        ctx_object_id(&mut b, 0, ObjectId::new(20, 1));
        ctx_enumerated(&mut b, 1, RECORD_COUNT);
        assert_eq!(
            read_range(&db, 0, &b, 1476),
            Ok(Reply::Error(BacError::PROPERTY_IS_NOT_A_LIST))
        );
        let mut b = Vec::new();
        ctx_object_id(&mut b, 0, ObjectId::new(0, 1));
        ctx_enumerated(&mut b, 1, LOG_BUFFER);
        assert_eq!(
            read_range(&db, 0, &b, 1476),
            Ok(Reply::Error(BacError::UNKNOWN_PROPERTY))
        );
    }

    #[test]
    fn writes() {
        let mut db = db(3, 100);
        let now = t0();
        let o = &mut db.devices[0].objects[1];
        assert_eq!(
            write(o, LOG_INTERVAL, &[0x21, 50], now),
            Err(BacError::VALUE_OUT_OF_RANGE)
        );
        assert!(write(o, LOG_INTERVAL, &[0x22, 0x01, 0xF4], now).is_ok());
        assert_eq!(o.as_trend_log().unwrap().interval, 500);
        assert!(write(o, ENABLE, &[0x10], now).is_ok());
        assert_eq!(o.as_trend_log().unwrap().record_count(), 4); // + log-disabled
        assert_eq!(
            write(o, RECORD_COUNT, &[0x21, 2], now),
            Err(BacError::VALUE_OUT_OF_RANGE)
        );
        assert!(write(o, RECORD_COUNT, &[0x21, 0], now).is_ok());
        assert_eq!(o.as_trend_log().unwrap().record_count(), 1); // buffer-purged
        assert_eq!(
            write(o, BUFFER_SIZE, &[0x21, 9], now),
            Err(BacError::WRITE_ACCESS_DENIED)
        );
    }
}
