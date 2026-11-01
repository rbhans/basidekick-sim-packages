//! The services a simulated device answers: property reads and writes,
//! Who-Is and Who-Has. COV subscriptions live in `cov`, since they outlast
//! the request that makes them; ReadRange lives in `trend`, and the alarm
//! services in `alarm`.
//!
//! Handlers take the decoded request body and the device it was addressed
//! to, and return a [`Reply`]; a body that doesn't decode comes back as a
//! [`DecodeError`] for the server to Reject. Present-value writes go through
//! bask-points so the simulation and the network share one set of rules.

use crate::apdu::{self, BacError, MAX_APDU, unconfirmed};
use crate::encoding::*;
use crate::properties::{self, Target, device_id, object_id, prop};
use crate::{alarm, schedule, trend};
use bask_points::{BacObject, ObjectType, PointDb, PointValue, WriteError, format_value};

/// A confirmed service's outcome, before it gets an invoke id.
#[derive(Debug, PartialEq)]
pub enum Reply {
    SimpleAck,
    ComplexAck(Vec<u8>),
    Error(BacError),
    /// WritePropertyMultiple's error, with the encoded reference to the write
    /// that failed.
    WpmError(BacError, Vec<u8>),
    Reject(u8),
}

impl Reply {
    pub fn into_apdu(self, invoke_id: u8, service: u8) -> Vec<u8> {
        match self {
            Reply::SimpleAck => apdu::simple_ack(invoke_id, service),
            Reply::ComplexAck(body) => apdu::complex_ack(invoke_id, service, &body),
            Reply::Error(e) => apdu::error(invoke_id, service, e),
            Reply::WpmError(e, failed) => apdu::wpm_error(invoke_id, e, &failed),
            Reply::Reject(reason) => apdu::reject(invoke_id, reason),
        }
    }
}

/// A write that took, for the server's event log.
#[derive(Clone, Debug, PartialEq)]
pub struct WriteDone {
    pub device: String,
    pub object: String,
    pub text: String,
}

// ---------------------------------------------------------------------------
// Reads

pub fn read_property(db: &PointDb, dev: usize, body: &[u8]) -> DResult<Reply> {
    let mut r = Reader::new(body);
    let id = r.ctx_object_id(0)?;
    let property = r.ctx_enumerated(1)?;
    let index = r.opt_ctx_unsigned(2)?;
    if !r.at_end() {
        return Err(DecodeError::TooManyArguments);
    }
    let d = &db.devices[dev];
    let id = properties::unwildcard(d, id);
    let value = match property {
        prop::ALL | prop::REQUIRED | prop::OPTIONAL => Err(BacError::READ_ACCESS_DENIED),
        _ => properties::resolve(d, id).and_then(|t| read_one(db, dev, t, property, index)),
    };
    let value = match value {
        Ok(v) => v,
        Err(e) => return Ok(Reply::Error(e)),
    };
    let mut ack = Vec::new();
    ctx_object_id(&mut ack, 0, id);
    ctx_enumerated(&mut ack, 1, property);
    if let Some(i) = index {
        ctx_unsigned(&mut ack, 2, i);
    }
    open(&mut ack, 3);
    ack.extend_from_slice(&value);
    close(&mut ack, 3);
    Ok(Reply::ComplexAck(ack))
}

fn read_one(
    db: &PointDb,
    dev: usize,
    t: Target,
    property: u32,
    index: Option<u64>,
) -> Result<Vec<u8>, BacError> {
    let mut v = Vec::new();
    properties::read(db, &db.devices[dev], t, property, index, &mut v)?;
    Ok(v)
}

pub fn read_property_multiple(db: &PointDb, dev: usize, body: &[u8]) -> DResult<Reply> {
    let d = &db.devices[dev];
    let mut r = Reader::new(body);
    let mut ack = Vec::new();
    loop {
        let id = properties::unwildcard(d, r.ctx_object_id(0)?);
        r.expect_open(1)?;
        let mut refs = Vec::new();
        while !r.peek_close(1) {
            refs.push((r.ctx_enumerated(0)?, r.opt_ctx_unsigned(1)?));
        }
        r.expect_close(1)?;
        if refs.is_empty() {
            return Err(DecodeError::Truncated);
        }

        let target = properties::resolve(d, id);
        ctx_object_id(&mut ack, 0, id);
        open(&mut ack, 1);
        for (property, index) in refs {
            let t = match target {
                Ok(t) => t,
                Err(e) => {
                    read_result(&mut ack, property, index, Err(e));
                    continue;
                }
            };
            let (required, optional) = properties::properties(d, t);
            let expanded = match property {
                prop::ALL => required.into_iter().chain(optional).collect(),
                prop::REQUIRED => required,
                prop::OPTIONAL => optional,
                _ => {
                    read_result(
                        &mut ack,
                        property,
                        index,
                        read_one(db, dev, t, property, index),
                    );
                    continue;
                }
            };
            for p in expanded {
                read_result(&mut ack, p, None, read_one(db, dev, t, p, None));
            }
        }
        close(&mut ack, 1);
        if r.at_end() {
            return Ok(Reply::ComplexAck(ack));
        }
    }
}

/// One entry of a ReadAccessResult's list of results.
fn read_result(
    ack: &mut Vec<u8>,
    property: u32,
    index: Option<u64>,
    value: Result<Vec<u8>, BacError>,
) {
    ctx_enumerated(ack, 2, property);
    if let Some(i) = index {
        ctx_unsigned(ack, 3, i);
    }
    match value {
        Ok(v) => {
            open(ack, 4);
            ack.extend_from_slice(&v);
            close(ack, 4);
        }
        Err(e) => {
            open(ack, 5);
            e.encode(ack);
            close(ack, 5);
        }
    }
}

// ---------------------------------------------------------------------------
// Writes

struct PropertyWrite {
    id: ObjectId,
    property: u32,
    index: Option<u64>,
    /// The encoded value, between its opening and closing tags.
    raw: Vec<u8>,
    priority: Option<u64>,
}

pub fn write_property(
    db: &mut PointDb,
    dev: usize,
    body: &[u8],
    done: &mut Vec<WriteDone>,
) -> DResult<Reply> {
    let mut r = Reader::new(body);
    let w = PropertyWrite {
        id: r.ctx_object_id(0)?,
        property: r.ctx_enumerated(1)?,
        index: r.opt_ctx_unsigned(2)?,
        raw: r.bracketed(3)?.to_vec(),
        priority: r.opt_ctx_unsigned(4)?,
    };
    if !r.at_end() {
        return Err(DecodeError::TooManyArguments);
    }
    Ok(match write(db, dev, &w) {
        Ok(d) => {
            done.push(d);
            Reply::SimpleAck
        }
        Err(e) => Reply::Error(e),
    })
}

/// Applies each write in order and stops at the first failure; the writes
/// before it stay applied, as the standard has it.
pub fn write_property_multiple(
    db: &mut PointDb,
    dev: usize,
    body: &[u8],
    done: &mut Vec<WriteDone>,
) -> DResult<Reply> {
    let mut r = Reader::new(body);
    let mut writes = Vec::new();
    loop {
        let id = r.ctx_object_id(0)?;
        r.expect_open(1)?;
        let before = writes.len();
        while !r.peek_close(1) {
            writes.push(PropertyWrite {
                id,
                property: r.ctx_enumerated(0)?,
                index: r.opt_ctx_unsigned(1)?,
                raw: r.bracketed(2)?.to_vec(),
                priority: r.opt_ctx_unsigned(3)?,
            });
        }
        r.expect_close(1)?;
        if writes.len() == before {
            return Err(DecodeError::Truncated);
        }
        if r.at_end() {
            break;
        }
    }
    for w in &writes {
        match write(db, dev, w) {
            Ok(d) => done.push(d),
            Err(e) => {
                let mut failed = Vec::new();
                ctx_object_id(
                    &mut failed,
                    0,
                    properties::unwildcard(&db.devices[dev], w.id),
                );
                ctx_enumerated(&mut failed, 1, w.property);
                if let Some(i) = w.index {
                    ctx_unsigned(&mut failed, 2, i);
                }
                return Ok(Reply::WpmError(e, failed));
            }
        }
    }
    Ok(Reply::SimpleAck)
}

fn write(db: &mut PointDb, dev: usize, w: &PropertyWrite) -> Result<WriteDone, BacError> {
    let now = db.time();
    let d = &mut db.devices[dev];
    let target = properties::resolve(d, w.id)?;
    if !properties::has_property(d, target, w.property) {
        return Err(BacError::UNKNOWN_PROPERTY);
    }
    if w.index.is_some() && !properties::is_array(w.property) {
        return Err(BacError::NOT_AN_ARRAY);
    }
    let device = d.name.clone();
    let (object, text) = match target {
        Target::Device => {
            if w.property != prop::DESCRIPTION {
                return Err(BacError::WRITE_ACCESS_DENIED);
            }
            d.description = char_string(&single_value(&w.raw)?)?;
            (d.name.clone(), format!("description = {:?}", d.description))
        }
        Target::Object(i) => {
            let o = &mut d.objects[i];
            let text = match (o.object_type, w.property) {
                (
                    _,
                    prop::PRESENT_VALUE
                    | prop::RELINQUISH_DEFAULT
                    | prop::OUT_OF_SERVICE
                    | prop::COV_INCREMENT
                    | prop::DESCRIPTION,
                ) => write_object(o, w.property, &single_value(&w.raw)?, w.priority)?,
                (ObjectType::Schedule, p) => schedule::write(o, p, w.index, &w.raw)?,
                (ObjectType::TrendLog, p) => trend::write(o, p, &w.raw, now)?,
                (ObjectType::NotificationClass, p) => alarm::write_class(o, p, w.index, &w.raw)?,
                (_, p) if o.reporting.is_some() => alarm::write_reporting(o, p, &w.raw)?,
                _ => return Err(BacError::WRITE_ACCESS_DENIED),
            };
            // A schedule follows its new settings (or its return to
            // service) at once, even with the sim paused.
            o.refresh_schedule(now);
            let object = o.name.clone();
            if o.object_type == ObjectType::Schedule && schedule::is_configuration(w.property) {
                d.database_revision = d.database_revision.wrapping_add(1);
            }
            (object, text)
        }
    };
    Ok(WriteDone {
        device,
        object,
        text,
    })
}

/// The one application-tagged value a simple property takes.
pub fn single_value(raw: &[u8]) -> Result<Value, BacError> {
    match app_values(raw) {
        Some(mut v) if v.len() == 1 => Ok(v.remove(0)),
        _ => Err(BacError::INVALID_DATA_TYPE),
    }
}

/// Applies a write to one of the writable object properties and describes it.
fn write_object(
    o: &mut BacObject,
    property: u32,
    value: &Value,
    priority: Option<u64>,
) -> Result<String, BacError> {
    match property {
        prop::PRESENT_VALUE => {
            let v = match value {
                Value::Null => None,
                v => Some(point_value(o.object_type, v)?),
            };
            let p = priority.map(|p| p.min(255) as u8);
            o.write_present_value(v, p).map_err(write_error)?;
            Ok(match v {
                Some(v) if o.commandable => {
                    format!(
                        "present-value = {} @ priority {}",
                        shown(o, v),
                        p.unwrap_or(16)
                    )
                }
                Some(v) => format!("present-value = {}", shown(o, v)),
                None => format!("relinquished priority {}", p.unwrap_or(16)),
            })
        }
        prop::RELINQUISH_DEFAULT => {
            let v = point_value(o.object_type, value)?;
            o.write_relinquish_default(v).map_err(write_error)?;
            Ok(format!("relinquish-default = {}", shown(o, v)))
        }
        prop::OUT_OF_SERVICE => {
            let Value::Boolean(b) = *value else {
                return Err(BacError::INVALID_DATA_TYPE);
            };
            o.out_of_service = b;
            Ok(format!("out-of-service = {b}"))
        }
        prop::COV_INCREMENT => {
            let x = number(value).ok_or(BacError::INVALID_DATA_TYPE)?;
            if !(x.is_finite() && x >= 0.0) {
                return Err(BacError::VALUE_OUT_OF_RANGE);
            }
            o.cov_increment = x;
            Ok(format!("cov-increment = {x}"))
        }
        prop::DESCRIPTION => {
            o.description = char_string(value)?;
            Ok(format!("description = {:?}", o.description))
        }
        _ => Err(BacError::WRITE_ACCESS_DENIED),
    }
}

/// A written value in the object's terms. Multi-state and binary range
/// checks are left to `BacObject::coerce`.
fn point_value(t: ObjectType, v: &Value) -> Result<PointValue, BacError> {
    let pv = if t.is_analog() {
        number(v).map(PointValue::Real)
    } else if t.is_binary() || t == ObjectType::Schedule {
        match *v {
            Value::Enumerated(e) => Some(PointValue::Multi(e)),
            Value::Boolean(b) => Some(PointValue::Binary(b)),
            _ => None,
        }
    } else {
        match *v {
            Value::Unsigned(u) => Some(PointValue::Multi(u.min(u32::MAX as u64) as u32)),
            _ => None,
        }
    };
    pv.ok_or(BacError::INVALID_DATA_TYPE)
}

fn number(v: &Value) -> Option<f32> {
    match *v {
        Value::Real(x) => Some(x),
        Value::Double(x) => Some(x as f32),
        Value::Unsigned(u) => Some(u as f32),
        Value::Signed(i) => Some(i as f32),
        _ => None,
    }
}

fn char_string(v: &Value) -> Result<String, BacError> {
    match v {
        Value::CharString(s) => Ok(s.clone()),
        _ => Err(BacError::INVALID_DATA_TYPE),
    }
}

/// A written value as the app shows it: `74.0 °F`, `Occupied`.
fn shown(o: &BacObject, v: PointValue) -> String {
    o.coerce(v)
        .map(|v| format_value(o, v))
        .unwrap_or_else(|_| format!("{v:?}"))
}

fn write_error(e: WriteError) -> BacError {
    match e {
        WriteError::WriteAccessDenied => BacError::WRITE_ACCESS_DENIED,
        WriteError::InvalidDataType => BacError::INVALID_DATA_TYPE,
        WriteError::ValueOutOfRange => BacError::VALUE_OUT_OF_RANGE,
    }
}

// ---------------------------------------------------------------------------
// Discovery

/// An inclusive device instance range; None matches every device.
pub type InstanceRange = Option<(u64, u64)>;

pub fn in_range(range: InstanceRange, instance: u32) -> bool {
    range.is_none_or(|(lo, hi)| (lo..=hi).contains(&(instance as u64)))
}

pub fn who_is_range(body: &[u8]) -> DResult<InstanceRange> {
    if body.is_empty() {
        return Ok(None);
    }
    let mut r = Reader::new(body);
    Ok(Some((r.ctx_unsigned(0)?, r.ctx_unsigned(1)?)))
}

pub fn i_am(db: &PointDb, dev: usize) -> Vec<u8> {
    let mut b = Vec::new();
    app_object_id(&mut b, device_id(&db.devices[dev]));
    app_unsigned(&mut b, MAX_APDU as u64);
    app_enumerated(&mut b, properties::SEGMENTATION_NONE);
    app_unsigned(&mut b, db.vendor_id as u64);
    apdu::unconfirmed_request(unconfirmed::I_AM, &b)
}

pub struct WhoHas {
    range: InstanceRange,
    object: Wanted,
}

enum Wanted {
    Id(ObjectId),
    Name(String),
}

pub fn parse_who_has(body: &[u8]) -> DResult<WhoHas> {
    let mut r = Reader::new(body);
    let range = match r.opt_ctx_unsigned(0)? {
        Some(lo) => Some((lo, r.ctx_unsigned(1)?)),
        None => None,
    };
    let object = if r.peek_context(2) {
        Wanted::Id(r.ctx_object_id(2)?)
    } else {
        Wanted::Name(r.ctx_char_string(3)?)
    };
    Ok(WhoHas { range, object })
}

/// I-Have from device `dev`, if it has what Who-Has asks for.
pub fn i_have(db: &PointDb, dev: usize, who_has: &WhoHas) -> Option<Vec<u8>> {
    let d = &db.devices[dev];
    if !in_range(who_has.range, d.instance) {
        return None;
    }
    let target = match &who_has.object {
        Wanted::Id(id) => properties::resolve(d, *id).ok()?,
        Wanted::Name(name) if *name == d.name => Target::Device,
        Wanted::Name(name) => Target::Object(d.find_by_name(name)?),
    };
    let (id, name) = match target {
        Target::Device => (device_id(d), &d.name),
        Target::Object(i) => (object_id(&d.objects[i]), &d.objects[i].name),
    };
    let mut b = Vec::new();
    app_object_id(&mut b, device_id(d));
    app_object_id(&mut b, id);
    app_char_string(&mut b, name);
    Some(apdu::unconfirmed_request(unconfirmed::I_HAVE, &b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bask_points::{BacDevice, Units};

    fn db() -> PointDb {
        let mut db = PointDb::new(2001);
        let mut av = BacObject::new(ObjectType::AnalogValue, 1, "ZN-SP", PointValue::Real(72.0));
        av.commandable = true;
        av.units = Units::DegreesFahrenheit;
        let ai = BacObject::new(ObjectType::AnalogInput, 1, "ZN-T", PointValue::Real(71.5));
        let bo = BacObject::new(
            ObjectType::BinaryOutput,
            1,
            "FAN-C",
            PointValue::Binary(false),
        );
        let mut msv = BacObject::new(
            ObjectType::MultiStateValue,
            1,
            "OCC-MODE",
            PointValue::Multi(1),
        );
        msv.commandable = true;
        msv.states = vec!["Auto".into(), "Occupied".into(), "Unoccupied".into()];
        db.devices.push(BacDevice {
            instance: 1001,
            name: "VAV-1".into(),
            description: String::new(),
            location: String::new(),
            model_name: "Sim VAV".into(),
            mac: Some(1),
            online: true,
            objects: vec![av, ai, bo, msv],
            database_revision: 1,
        });
        db
    }

    fn wp(
        id: ObjectId,
        property: u32,
        value: impl FnOnce(&mut Vec<u8>),
        priority: Option<u64>,
    ) -> Vec<u8> {
        let mut b = Vec::new();
        ctx_object_id(&mut b, 0, id);
        ctx_enumerated(&mut b, 1, property);
        open(&mut b, 3);
        value(&mut b);
        close(&mut b, 3);
        if let Some(p) = priority {
            ctx_unsigned(&mut b, 4, p);
        }
        b
    }

    #[test]
    fn writes_follow_points_rules() {
        let mut db = db();
        let mut done = Vec::new();
        let av = ObjectId::new(2, 1);
        let req = wp(av, prop::PRESENT_VALUE, |b| app_real(b, 74.0), Some(8));
        assert_eq!(
            write_property(&mut db, 0, &req, &mut done),
            Ok(Reply::SimpleAck)
        );
        assert_eq!(done[0].text, "present-value = 74.0 °F @ priority 8");
        assert_eq!(db.devices[0].objects[0].active_priority(), Some(8));
        let req = wp(av, prop::PRESENT_VALUE, app_null, Some(8));
        assert_eq!(
            write_property(&mut db, 0, &req, &mut done),
            Ok(Reply::SimpleAck)
        );
        assert_eq!(done[1].text, "relinquished priority 8");
        // Unsigned is accepted for analog values
        let req = wp(av, prop::RELINQUISH_DEFAULT, |b| app_unsigned(b, 70), None);
        assert_eq!(
            write_property(&mut db, 0, &req, &mut done),
            Ok(Reply::SimpleAck)
        );
        assert_eq!(
            db.devices[0].objects[0].present_value,
            PointValue::Real(70.0)
        );

        let ai = ObjectId::new(0, 1);
        let req = wp(ai, prop::PRESENT_VALUE, |b| app_real(b, 60.0), None);
        assert_eq!(
            write_property(&mut db, 0, &req, &mut done),
            Ok(Reply::Error(BacError::WRITE_ACCESS_DENIED))
        );
        let req = wp(ai, prop::OUT_OF_SERVICE, |b| app_bool(b, true), None);
        assert_eq!(
            write_property(&mut db, 0, &req, &mut done),
            Ok(Reply::SimpleAck)
        );
        let req = wp(ai, prop::PRESENT_VALUE, |b| app_real(b, 60.0), None);
        assert_eq!(
            write_property(&mut db, 0, &req, &mut done),
            Ok(Reply::SimpleAck)
        );
        assert_eq!(done.last().unwrap().text, "present-value = 60.0");

        let bo = ObjectId::new(4, 1);
        let req = wp(bo, prop::PRESENT_VALUE, |b| app_enumerated(b, 1), Some(10));
        assert_eq!(
            write_property(&mut db, 0, &req, &mut done),
            Ok(Reply::SimpleAck)
        );
        assert_eq!(
            done.last().unwrap().text,
            "present-value = On @ priority 10"
        );
        let req = wp(bo, prop::PRESENT_VALUE, |b| app_real(b, 1.0), Some(10));
        assert_eq!(
            write_property(&mut db, 0, &req, &mut done),
            Ok(Reply::Error(BacError::INVALID_DATA_TYPE))
        );
        let req = wp(bo, prop::PRESENT_VALUE, |b| app_enumerated(b, 2), Some(10));
        assert_eq!(
            write_property(&mut db, 0, &req, &mut done),
            Ok(Reply::Error(BacError::VALUE_OUT_OF_RANGE))
        );

        let msv = ObjectId::new(19, 1);
        let req = wp(msv, prop::PRESENT_VALUE, |b| app_unsigned(b, 2), Some(8));
        assert_eq!(
            write_property(&mut db, 0, &req, &mut done),
            Ok(Reply::SimpleAck)
        );
        assert_eq!(
            done.last().unwrap().text,
            "present-value = Occupied @ priority 8"
        );
        let req = wp(msv, prop::PRESENT_VALUE, |b| app_unsigned(b, 0), Some(8));
        assert_eq!(
            write_property(&mut db, 0, &req, &mut done),
            Ok(Reply::Error(BacError::VALUE_OUT_OF_RANGE))
        );
        let req = wp(msv, prop::PRESENT_VALUE, |b| app_unsigned(b, 2), Some(6));
        assert_eq!(
            write_property(&mut db, 0, &req, &mut done),
            Ok(Reply::Error(BacError::VALUE_OUT_OF_RANGE))
        );

        let req = wp(msv, prop::OBJECT_NAME, |b| app_char_string(b, "X"), None);
        assert_eq!(
            write_property(&mut db, 0, &req, &mut done),
            Ok(Reply::Error(BacError::WRITE_ACCESS_DENIED))
        );
        let req = wp(msv, prop::UNITS, |b| app_enumerated(b, 1), None);
        assert_eq!(
            write_property(&mut db, 0, &req, &mut done),
            Ok(Reply::Error(BacError::UNKNOWN_PROPERTY))
        );
        let req = wp(
            ObjectId::new(8, ObjectId::WILDCARD),
            prop::DESCRIPTION,
            |b| app_char_string(b, "Room 101"),
            None,
        );
        assert_eq!(
            write_property(&mut db, 0, &req, &mut done),
            Ok(Reply::SimpleAck)
        );
        assert_eq!(db.devices[0].description, "Room 101");
        assert_eq!(
            write_property(&mut db, 0, &req[..4], &mut done),
            Err(DecodeError::Truncated)
        );
    }

    #[test]
    fn wpm_stops_at_first_failure() {
        let mut db = db();
        let mut b = Vec::new();
        ctx_object_id(&mut b, 0, ObjectId::new(2, 1));
        open(&mut b, 1);
        ctx_enumerated(&mut b, 0, prop::PRESENT_VALUE);
        open(&mut b, 2);
        app_real(&mut b, 68.0);
        close(&mut b, 2);
        ctx_unsigned(&mut b, 3, 9);
        ctx_enumerated(&mut b, 0, prop::OBJECT_NAME);
        open(&mut b, 2);
        app_char_string(&mut b, "nope");
        close(&mut b, 2);
        close(&mut b, 1);
        let mut done = Vec::new();
        let Ok(Reply::WpmError(e, failed)) = write_property_multiple(&mut db, 0, &b, &mut done)
        else {
            panic!()
        };
        assert_eq!(e, BacError::WRITE_ACCESS_DENIED);
        assert_eq!(failed, [0x0C, 0x00, 0x80, 0x00, 0x01, 0x19, 77]);
        assert_eq!(done.len(), 1);
        assert_eq!(db.devices[0].objects[0].active_priority(), Some(9));
    }

    #[test]
    fn rpm_reports_errors_inline() {
        let db = db();
        let mut b = Vec::new();
        ctx_object_id(&mut b, 0, ObjectId::new(0, 1));
        open(&mut b, 1);
        ctx_enumerated(&mut b, 0, prop::PRESENT_VALUE);
        ctx_enumerated(&mut b, 0, prop::PRIORITY_ARRAY);
        close(&mut b, 1);
        ctx_object_id(&mut b, 0, ObjectId::new(0, 9));
        open(&mut b, 1);
        ctx_enumerated(&mut b, 0, prop::ALL);
        close(&mut b, 1);
        let Ok(Reply::ComplexAck(ack)) = read_property_multiple(&db, 0, &b) else {
            panic!()
        };
        let mut expect = Vec::new();
        ctx_object_id(&mut expect, 0, ObjectId::new(0, 1));
        open(&mut expect, 1);
        ctx_enumerated(&mut expect, 2, prop::PRESENT_VALUE);
        open(&mut expect, 4);
        app_real(&mut expect, 71.5);
        close(&mut expect, 4);
        ctx_enumerated(&mut expect, 2, prop::PRIORITY_ARRAY);
        open(&mut expect, 5);
        BacError::UNKNOWN_PROPERTY.encode(&mut expect);
        close(&mut expect, 5);
        close(&mut expect, 1);
        ctx_object_id(&mut expect, 0, ObjectId::new(0, 9));
        open(&mut expect, 1);
        ctx_enumerated(&mut expect, 2, prop::ALL);
        open(&mut expect, 5);
        BacError::UNKNOWN_OBJECT.encode(&mut expect);
        close(&mut expect, 5);
        close(&mut expect, 1);
        assert_eq!(ack, expect);
        assert_eq!(
            read_property_multiple(&db, 0, &b[..b.len() - 1]),
            Err(DecodeError::Truncated)
        );
    }

    #[test]
    fn who_has_by_name_and_id() {
        let db = db();
        let mut b = Vec::new();
        b.push(0x3D); // context 3, extended length
        b.push(9);
        b.push(0);
        b.extend_from_slice(b"OCC-MODE");
        let wh = parse_who_has(&b).unwrap();
        let ihave = i_have(&db, 0, &wh).unwrap();
        assert_eq!(&ihave[..2], [0x10, unconfirmed::I_HAVE]);
        assert!(ihave.ends_with(b"OCC-MODE"));
        let mut b = Vec::new();
        ctx_unsigned(&mut b, 0, 2000);
        ctx_unsigned(&mut b, 1, 3000);
        ctx_object_id(&mut b, 2, ObjectId::new(2, 1));
        assert!(i_have(&db, 0, &parse_who_has(&b).unwrap()).is_none());
        assert_eq!(
            who_is_range(&[0x09, 0x00, 0x1A, 0x03, 0xE9]),
            Ok(Some((0, 1001)))
        );
        assert!(in_range(Some((0, 1001)), 1001) && !in_range(Some((0, 1000)), 1001));
    }
}
