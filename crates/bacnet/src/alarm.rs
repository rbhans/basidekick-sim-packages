//! Alarms: intrinsic reporting and notification class properties, event
//! notifications, AcknowledgeAlarm (13.5) and GetEventInformation (13.12).
//!
//! The database decides when a point changes event state and queues an
//! [`EventNotice`]. The server drains the queue each tick and hands every
//! notice here, which builds the notification for each destination in the
//! point's notification class whose days, times and transitions match.
//!
//! Device recipients are found with Who-Is (a global broadcast, repeated
//! every half minute while unanswered) and remembered from their I-Am.
//! Until one answers, notifications meant for it are skipped, not broadcast:
//! a confirmed notification can't be broadcast, and a front end that asked
//! for it by device would get duplicates the day it answers. Confirmed
//! notifications are sent once; the ack (or error, or silence) is ignored.

use crate::apdu::{self, BacError, confirmed, unconfirmed};
use crate::encoding::*;
use crate::npdu::{NetAddr, Peer};
use crate::properties::{self, Target, encode_value, object_id, prop::*};
use crate::services::{Reply, single_value};
use bask_points::{
    BacDevice, BacObject, Destination, EventAlgorithm, EventNotice, EventState, EventValues,
    NotificationClass, NotifyType, ObjectType, PointDb, PointRef, Recipient,
};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::{Duration, Instant};

/// Destinations one notification class may hold.
const MAX_RECIPIENTS: usize = 16;
/// How often Who-Is goes out again for a recipient that hasn't answered.
const REBIND: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Properties

/// Non-array alarm properties of a reporting point or a notification class.
/// False for anything else.
pub fn read(o: &BacObject, property: u32, buf: &mut Vec<u8>) -> bool {
    if let Some(nc) = o.as_notification_class() {
        match property {
            NOTIFICATION_CLASS => app_unsigned(buf, o.instance as u64),
            ACK_REQUIRED => app_bit_string(buf, &nc.ack_required),
            RECIPIENT_LIST => nc.recipients.iter().for_each(|d| destination(buf, d)),
            _ => return false,
        }
        return true;
    }
    let Some(r) = o.reporting.as_deref() else {
        return false;
    };
    match (property, r.algorithm) {
        (NOTIFICATION_CLASS, _) => app_unsigned(buf, r.notification_class as u64),
        (TIME_DELAY, _) => app_unsigned(buf, r.time_delay as u64),
        (EVENT_ENABLE, _) => app_bit_string(buf, &r.event_enable),
        (ACKED_TRANSITIONS, _) => app_bit_string(buf, &r.acked),
        (NOTIFY_TYPE, _) => app_enumerated(buf, r.notify_type.bacnet_id()),
        (EVENT_DETECTION_ENABLE, _) => app_bool(buf, r.detection_enable),
        (ALARM_VALUE, EventAlgorithm::ChangeOfState { alarm_value }) => {
            app_enumerated(buf, alarm_value as u32)
        }
        (HIGH_LIMIT, EventAlgorithm::OutOfRange { high, .. }) => app_real(buf, high),
        (LOW_LIMIT, EventAlgorithm::OutOfRange { low, .. }) => app_real(buf, low),
        (DEADBAND, EventAlgorithm::OutOfRange { deadband, .. }) => app_real(buf, deadband),
        (
            LIMIT_ENABLE,
            EventAlgorithm::OutOfRange {
                low_enable,
                high_enable,
                ..
            },
        ) => app_bit_string(buf, &[low_enable, high_enable]),
        _ => return false,
    }
    true
}

pub fn array_len(o: &BacObject, property: u32) -> Option<usize> {
    match property {
        EVENT_TIME_STAMPS if o.reporting.is_some() => Some(3),
        PRIORITY if o.as_notification_class().is_some() => Some(3),
        _ => None,
    }
}

/// One element (0-based) of Event_Time_Stamps or Priority.
pub fn element(o: &BacObject, property: u32, i: usize, buf: &mut Vec<u8>) -> bool {
    match (property, o.reporting.as_deref(), o.as_notification_class()) {
        (EVENT_TIME_STAMPS, Some(r), _) => time_stamp(buf, r.time_stamps[i]),
        (PRIORITY, _, Some(nc)) => app_unsigned(buf, nc.priority[i] as u64),
        _ => return false,
    }
    true
}

/// BACnetDestination.
fn destination(buf: &mut Vec<u8>, d: &Destination) {
    app_bit_string(buf, &d.days);
    app_time(buf, time_octets(d.from));
    app_time(buf, time_octets(d.to));
    match &d.recipient {
        Recipient::Device(i) => ctx_object_id(buf, 0, ObjectId::new(ObjectId::DEVICE, *i)),
        Recipient::Address { network, mac } => {
            open(buf, 1);
            app_unsigned(buf, *network as u64);
            app_octet_string(buf, mac);
            close(buf, 1);
        }
    }
    app_unsigned(buf, d.process_id as u64);
    app_bool(buf, d.confirmed);
    app_bit_string(buf, &d.transitions);
}

fn bad(_: DecodeError) -> BacError {
    BacError::INVALID_DATA_TYPE
}

fn bits(v: Value, n: usize) -> Result<Vec<bool>, BacError> {
    match v {
        Value::BitString(b) if b.len() >= n => Ok(b[..n].to_vec()),
        Value::BitString(_) => Err(BacError::VALUE_OUT_OF_RANGE),
        _ => Err(BacError::INVALID_DATA_TYPE),
    }
}

fn decode_destination(r: &mut Reader) -> Result<Destination, BacError> {
    let mut app = || r.read_app().map_err(bad);
    let days = bits(app()?, 7)?;
    let time = |v: Value| match v {
        Value::Time(t) => time_of(t).ok_or(BacError::VALUE_OUT_OF_RANGE),
        _ => Err(BacError::INVALID_DATA_TYPE),
    };
    let from = time(app()?)?;
    let to = time(app()?)?;
    let recipient = if r.peek_context(0) {
        let id = r.ctx_object_id(0).map_err(bad)?;
        if id.object_type != ObjectId::DEVICE {
            return Err(BacError::VALUE_OUT_OF_RANGE);
        }
        Recipient::Device(id.instance)
    } else {
        r.expect_open(1).map_err(bad)?;
        let (Value::Unsigned(network), Value::OctetString(mac)) =
            (r.read_app().map_err(bad)?, r.read_app().map_err(bad)?)
        else {
            return Err(BacError::INVALID_DATA_TYPE);
        };
        r.expect_close(1).map_err(bad)?;
        let network = u16::try_from(network).map_err(|_| BacError::VALUE_OUT_OF_RANGE)?;
        Recipient::Address { network, mac }
    };
    let mut app = || r.read_app().map_err(bad);
    let (Value::Unsigned(process_id), Value::Boolean(confirmed)) = (app()?, app()?) else {
        return Err(BacError::INVALID_DATA_TYPE);
    };
    let transitions = bits(app()?, 3)?;
    Ok(Destination {
        days: days.try_into().unwrap(),
        from,
        to,
        recipient,
        process_id: u32::try_from(process_id).map_err(|_| BacError::VALUE_OUT_OF_RANGE)?,
        confirmed,
        transitions: transitions.try_into().unwrap(),
    })
}

/// Applies a write to a notification class's Priority, Ack_Required or
/// Recipient_List.
pub fn write_class(
    o: &mut BacObject,
    property: u32,
    index: Option<u64>,
    raw: &[u8],
) -> Result<String, BacError> {
    let nc = o
        .as_notification_class_mut()
        .ok_or(BacError::WRITE_ACCESS_DENIED)?;
    let priority = |v: &Value| match *v {
        Value::Unsigned(p) if p <= 255 => Ok(p as u8),
        Value::Unsigned(_) => Err(BacError::VALUE_OUT_OF_RANGE),
        _ => Err(BacError::INVALID_DATA_TYPE),
    };
    match (property, index) {
        (PRIORITY, None) => {
            let values = app_values(raw).ok_or(BacError::INVALID_DATA_TYPE)?;
            let [a, b, c] = &values[..] else {
                return Err(BacError::INVALID_DATA_TYPE);
            };
            nc.priority = [priority(a)?, priority(b)?, priority(c)?];
            Ok(format!("priority = {:?}", nc.priority))
        }
        (PRIORITY, Some(0)) => Err(BacError::WRITE_ACCESS_DENIED),
        (PRIORITY, Some(i @ 1..=3)) => {
            nc.priority[i as usize - 1] = priority(&single_value(raw)?)?;
            Ok(format!("priority[{i}] = {}", nc.priority[i as usize - 1]))
        }
        (PRIORITY, Some(_)) => Err(BacError::INVALID_ARRAY_INDEX),
        (ACK_REQUIRED, None) => {
            nc.ack_required = bits(single_value(raw)?, 3)?.try_into().unwrap();
            Ok(format!(
                "ack-required = {}",
                transition_bits(nc.ack_required)
            ))
        }
        (RECIPIENT_LIST, None) => {
            let mut r = Reader::new(raw);
            let mut list = Vec::new();
            while !r.at_end() {
                list.push(decode_destination(&mut r)?);
                if list.len() > MAX_RECIPIENTS {
                    return Err(BacError::NO_SPACE_TO_WRITE);
                }
            }
            nc.recipients = list;
            Ok(format!(
                "recipient-list = {} recipients",
                nc.recipients.len()
            ))
        }
        _ => Err(BacError::WRITE_ACCESS_DENIED),
    }
}

/// Applies a write to one of a reporting point's alarm settings.
pub fn write_reporting(o: &mut BacObject, property: u32, raw: &[u8]) -> Result<String, BacError> {
    let r = o
        .reporting
        .as_deref_mut()
        .ok_or(BacError::WRITE_ACCESS_DENIED)?;
    let writable = matches!(
        property,
        NOTIFICATION_CLASS
            | TIME_DELAY
            | EVENT_ENABLE
            | NOTIFY_TYPE
            | EVENT_DETECTION_ENABLE
            | ALARM_VALUE
            | HIGH_LIMIT
            | LOW_LIMIT
            | DEADBAND
            | LIMIT_ENABLE
    );
    if !writable {
        return Err(BacError::WRITE_ACCESS_DENIED);
    }
    let v = single_value(raw)?;
    let unsigned = |v: &Value| match *v {
        Value::Unsigned(n) => u32::try_from(n).map_err(|_| BacError::VALUE_OUT_OF_RANGE),
        _ => Err(BacError::INVALID_DATA_TYPE),
    };
    let real = |v: &Value| match *v {
        Value::Real(x) if x.is_finite() => Ok(x),
        Value::Double(x) if x.is_finite() => Ok(x as f32),
        Value::Real(_) | Value::Double(_) => Err(BacError::VALUE_OUT_OF_RANGE),
        _ => Err(BacError::INVALID_DATA_TYPE),
    };
    let text = match (property, &mut r.algorithm) {
        (NOTIFICATION_CLASS, _) => {
            r.notification_class = unsigned(&v)?;
            format!("notification-class = {}", r.notification_class)
        }
        (TIME_DELAY, _) => {
            r.time_delay = unsigned(&v)?;
            format!("time-delay = {} s", r.time_delay)
        }
        (EVENT_ENABLE, _) => {
            r.event_enable = bits(v, 3)?.try_into().unwrap();
            format!("event-enable = {}", transition_bits(r.event_enable))
        }
        (NOTIFY_TYPE, _) => {
            r.notify_type = match v {
                Value::Enumerated(0) => NotifyType::Alarm,
                Value::Enumerated(1) => NotifyType::Event,
                Value::Enumerated(_) => return Err(BacError::VALUE_OUT_OF_RANGE),
                _ => return Err(BacError::INVALID_DATA_TYPE),
            };
            format!("notify-type = {:?}", r.notify_type).to_lowercase()
        }
        (EVENT_DETECTION_ENABLE, _) => {
            let Value::Boolean(b) = v else {
                return Err(BacError::INVALID_DATA_TYPE);
            };
            r.set_detection_enable(b);
            format!("event-detection-enable = {b}")
        }
        (ALARM_VALUE, EventAlgorithm::ChangeOfState { alarm_value }) => {
            *alarm_value = match v {
                Value::Enumerated(e @ (0 | 1)) => e == 1,
                Value::Enumerated(_) => return Err(BacError::VALUE_OUT_OF_RANGE),
                _ => return Err(BacError::INVALID_DATA_TYPE),
            };
            format!("alarm-value = {}", *alarm_value as u8)
        }
        (HIGH_LIMIT, EventAlgorithm::OutOfRange { high, .. }) => {
            *high = real(&v)?;
            format!("high-limit = {high}")
        }
        (LOW_LIMIT, EventAlgorithm::OutOfRange { low, .. }) => {
            *low = real(&v)?;
            format!("low-limit = {low}")
        }
        (DEADBAND, EventAlgorithm::OutOfRange { deadband, .. }) => {
            let x = real(&v)?;
            if x < 0.0 {
                return Err(BacError::VALUE_OUT_OF_RANGE);
            }
            *deadband = x;
            format!("deadband = {x}")
        }
        (
            LIMIT_ENABLE,
            EventAlgorithm::OutOfRange {
                low_enable,
                high_enable,
                ..
            },
        ) => {
            let b = bits(v, 2)?;
            (*low_enable, *high_enable) = (b[0], b[1]);
            format!("limit-enable = low {}, high {}", b[0], b[1])
        }
        _ => return Err(BacError::WRITE_ACCESS_DENIED),
    };
    Ok(text)
}

/// `to-offnormal, to-fault` and so on.
fn transition_bits(b: [bool; 3]) -> String {
    let names = ["to-offnormal", "to-fault", "to-normal"];
    let on: Vec<&str> = names
        .iter()
        .zip(b)
        .filter(|(_, b)| *b)
        .map(|(n, _)| *n)
        .collect();
    if on.is_empty() {
        "none".into()
    } else {
        on.join(", ")
    }
}

// ---------------------------------------------------------------------------
// AcknowledgeAlarm and GetEventInformation

/// A BACnetTimeStamp as a request quotes it.
enum Stamp {
    Time([u8; 4]),
    Sequence,
    DateTime([u8; 4], [u8; 4]),
}

fn decode_stamp(r: &mut Reader, number: u8) -> DResult<Stamp> {
    r.expect_open(number)?;
    let stamp = if r.peek_open(2) {
        r.expect_open(2)?;
        let (Value::Date(d), Value::Time(t)) = (r.read_app()?, r.read_app()?) else {
            return Err(DecodeError::BadValue);
        };
        r.expect_close(2)?;
        Stamp::DateTime(d, t)
    } else if r.peek_context(1) {
        r.ctx_unsigned(1)?;
        Stamp::Sequence
    } else {
        Stamp::Time(
            r.ctx_raw(0)?
                .try_into()
                .map_err(|_| DecodeError::BadValue)?,
        )
    };
    r.expect_close(number)?;
    Ok(stamp)
}

/// Whether a quoted time stamp is the one we recorded, octet for octet
/// (unspecified octets match anything; the weekday isn't compared).
fn same_stamp(stamp: &Stamp, ours: chrono::NaiveDateTime) -> bool {
    let eq = |a: &[u8], b: &[u8]| a.iter().zip(b).all(|(x, y)| *x == 255 || x == y);
    match stamp {
        Stamp::Time(t) => eq(t, &time_octets(ours.time())),
        Stamp::DateTime(d, t) => {
            eq(&d[..3], &date_octets(ours.date())[..3]) && eq(t, &time_octets(ours.time()))
        }
        Stamp::Sequence => false,
    }
}

/// AcknowledgeAlarm: on success, the name of the object acknowledged.
pub fn acknowledge(db: &mut PointDb, dev: usize, body: &[u8]) -> DResult<Result<String, BacError>> {
    let mut r = Reader::new(body);
    r.ctx_unsigned(0)?; // acknowledging process
    let id = r.ctx_object_id(1)?;
    let state = r.ctx_enumerated(2)?;
    let stamp = decode_stamp(&mut r, 3)?;
    r.ctx_char_string(4)?; // acknowledgment source
    decode_stamp(&mut r, 5)?; // time of acknowledgment
    if !r.at_end() {
        return Err(DecodeError::TooManyArguments);
    }
    let d = &db.devices[dev];
    let i = match properties::resolve(d, id) {
        Ok(Target::Object(i)) => i,
        Ok(Target::Device) => return Ok(Err(BacError::NO_ALARM_CONFIGURED)),
        Err(e) => return Ok(Err(e)),
    };
    let o = &d.objects[i];
    let Some(rep) = o.reporting.as_deref() else {
        return Ok(Err(BacError::NO_ALARM_CONFIGURED));
    };
    let Some(state) = EventState::from_bacnet_id(state) else {
        return Ok(Err(BacError::VALUE_OUT_OF_RANGE));
    };
    let t = bask_points::Transition::to(state).index();
    match rep.time_stamps[t] {
        Some(ours) if same_stamp(&stamp, ours) => {}
        _ => return Ok(Err(BacError::INVALID_TIME_STAMP)),
    }
    let name = o.name.clone();
    // Acknowledging what's already acknowledged is fine too.
    db.acknowledge(
        PointRef {
            device: dev as u16,
            object: i as u16,
        },
        state,
    );
    Ok(Ok(name))
}

pub fn get_event_information(
    db: &PointDb,
    dev: usize,
    body: &[u8],
    max_apdu: usize,
) -> DResult<Reply> {
    let mut r = Reader::new(body);
    let last = if r.peek_context(0) {
        Some(r.ctx_object_id(0)?)
    } else {
        None
    };
    if !r.at_end() {
        return Err(DecodeError::TooManyArguments);
    }
    let d = &db.devices[dev];
    // Carry on after the last object the client got, wherever it is now.
    let start = last
        .and_then(|id| d.objects.iter().position(|o| object_id(o) == id))
        .map_or(0, |p| p + 1);
    // APDU header, the list's brackets and more-events.
    let budget = max_apdu.saturating_sub(3 + 2 + 2);
    let mut list = Vec::new();
    let mut more = false;
    for o in &d.objects[start..] {
        let Some(rep) = o.reporting.as_deref() else {
            continue;
        };
        if rep.state == EventState::Normal && !rep.unacknowledged() {
            continue;
        }
        let mut s = Vec::new();
        ctx_object_id(&mut s, 0, object_id(o));
        ctx_enumerated(&mut s, 1, rep.state.bacnet_id());
        ctx_bit_string(&mut s, 2, &rep.acked);
        open(&mut s, 3);
        rep.time_stamps.iter().for_each(|t| time_stamp(&mut s, *t));
        close(&mut s, 3);
        ctx_enumerated(&mut s, 4, rep.notify_type.bacnet_id());
        ctx_bit_string(&mut s, 5, &rep.event_enable);
        open(&mut s, 6);
        priorities(d, rep.notification_class)
            .iter()
            .for_each(|p| app_unsigned(&mut s, *p as u64));
        close(&mut s, 6);
        if list.len() + s.len() > budget {
            more = true;
            break;
        }
        list.extend(s);
    }
    let mut ack = Vec::new();
    open(&mut ack, 0);
    ack.extend(list);
    close(&mut ack, 0);
    ctx_bool(&mut ack, 1, more);
    Ok(Reply::ComplexAck(ack))
}

fn priorities(d: &BacDevice, class: u32) -> [u8; 3] {
    d.notification_class(class)
        .map_or([255; 3], |nc| nc.priority)
}

// ---------------------------------------------------------------------------
// Notifications

/// The parameters of an Event Notification for `n` to process `process_id`.
pub fn notification(n: &EventNotice, process_id: u32) -> Vec<u8> {
    let mut b = Vec::new();
    ctx_unsigned(&mut b, 0, process_id as u64);
    ctx_object_id(&mut b, 1, ObjectId::new(ObjectId::DEVICE, n.device));
    ctx_object_id(
        &mut b,
        2,
        ObjectId::new(n.object_type.bacnet_id(), n.instance),
    );
    open(&mut b, 3);
    time_stamp(&mut b, Some(n.time));
    close(&mut b, 3);
    ctx_unsigned(&mut b, 4, n.notification_class as u64);
    ctx_unsigned(&mut b, 5, n.priority as u64);
    ctx_enumerated(&mut b, 6, n.event_type.bacnet_id());
    ctx_char_string(&mut b, 7, &n.text);
    ctx_enumerated(&mut b, 8, n.notify_type.bacnet_id());
    if n.notify_type != NotifyType::AckNotification {
        ctx_bool(&mut b, 9, n.ack_required);
        ctx_enumerated(&mut b, 10, n.from.bacnet_id());
    }
    ctx_enumerated(&mut b, 11, n.to.bacnet_id());
    if let Some(values) = n.values {
        open(&mut b, 12);
        event_values(&mut b, n.object_type, values);
        close(&mut b, 12);
    }
    b
}

/// BACnetNotificationParameters.
fn event_values(b: &mut Vec<u8>, t: ObjectType, values: EventValues) {
    match values {
        EventValues::ChangeOfState { value, flags } => {
            open(b, 1);
            open(b, 0);
            ctx_enumerated(b, 1, value as u32); // binary-value
            close(b, 0);
            ctx_bit_string(b, 1, &flags);
            close(b, 1);
        }
        EventValues::OutOfRange {
            value,
            flags,
            deadband,
            limit,
        } => {
            open(b, 5);
            ctx_real(b, 0, value);
            ctx_bit_string(b, 1, &flags);
            ctx_real(b, 2, deadband);
            ctx_real(b, 3, limit);
            close(b, 5);
        }
        EventValues::ChangeOfReliability {
            reliability,
            flags,
            value,
        } => {
            open(b, 19);
            ctx_enumerated(b, 0, reliability.bacnet_id());
            ctx_bit_string(b, 1, &flags);
            open(b, 2);
            ctx_enumerated(b, 0, PRESENT_VALUE);
            open(b, 2);
            encode_value(b, t, value);
            close(b, 2);
            close(b, 2);
            close(b, 19);
        }
    }
}

/// Where one notification goes.
#[derive(Debug, PartialEq)]
pub enum Route {
    /// Straight to a station on BACnet/IP, or through its router.
    Peer(Peer),
    LocalBroadcast,
    /// A station or broadcast on another network, through whatever router
    /// hears a local broadcast for it.
    Remote(NetAddr),
}

/// A notification ready to send from device `device`.
#[derive(Debug)]
pub struct Outgoing {
    pub device: usize,
    pub route: Route,
    pub apdu: Vec<u8>,
    pub confirmed: bool,
}

/// Device recipients found with Who-Is, and the invoke ids of confirmed
/// notifications.
#[derive(Default)]
pub struct Notifier {
    found: HashMap<u32, Peer>,
    asked: HashMap<u32, Instant>,
    invoke_id: u8,
}

impl Notifier {
    /// Notes an I-Am from a device some notification class sends to.
    pub fn i_am(&mut self, db: &PointDb, body: &[u8], peer: &Peer) {
        let Ok(Value::ObjectId(id)) = Reader::new(body).read_app() else {
            return;
        };
        if id.object_type == ObjectId::DEVICE && wanted(db).any(|i| i == id.instance) {
            self.found.insert(id.instance, peer.clone());
        }
    }

    /// Device recipients still to be found and due another Who-Is.
    pub fn unbound(&mut self, db: &PointDb, now: Instant) -> Vec<u32> {
        let mut due: Vec<u32> = wanted(db)
            .filter(|i| !self.found.contains_key(i))
            .filter(|i| {
                self.asked
                    .get(i)
                    .is_none_or(|t| now.duration_since(*t) >= REBIND)
            })
            .collect();
        due.sort_unstable();
        due.dedup();
        for i in &due {
            self.asked.insert(*i, now);
        }
        due
    }

    /// The notifications for `n`, one per matching destination.
    pub fn route(&mut self, db: &PointDb, n: &EventNotice) -> Vec<Outgoing> {
        let Some(dev) = db.device_index(n.device) else {
            return Vec::new();
        };
        let Some(nc) = db.devices[dev].notification_class(n.notification_class) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for dest in nc_destinations(nc, n) {
            let route = match &dest.recipient {
                // Our own devices don't take notifications.
                Recipient::Device(i) if db.device_index(*i).is_some() => continue,
                Recipient::Device(i) => match self.found.get(i) {
                    Some(peer) => Route::Peer(peer.clone()),
                    None => continue,
                },
                Recipient::Address { network: 0, mac } => match mac[..] {
                    [] => Route::LocalBroadcast,
                    [a, b, c, d, hi, lo] => Route::Peer(Peer {
                        addr: SocketAddrV4::new(
                            Ipv4Addr::new(a, b, c, d),
                            u16::from_be_bytes([hi, lo]),
                        ),
                        remote: None,
                    }),
                    _ => continue,
                },
                Recipient::Address { network, .. } if *network == db.network => continue,
                Recipient::Address { network, mac } => Route::Remote(NetAddr {
                    net: *network,
                    mac: mac.clone(),
                }),
            };
            let broadcast = match &route {
                Route::LocalBroadcast => true,
                Route::Remote(a) => a.mac.is_empty(),
                Route::Peer(_) => false,
            };
            // A broadcast can't be confirmed.
            let confirmed = dest.confirmed && !broadcast;
            let body = notification(n, dest.process_id);
            let apdu = if confirmed {
                self.invoke_id = self.invoke_id.wrapping_add(1);
                apdu::confirmed_request(self.invoke_id, confirmed::EVENT_NOTIFICATION, &body)
            } else {
                apdu::unconfirmed_request(unconfirmed::EVENT_NOTIFICATION, &body)
            };
            out.push(Outgoing {
                device: dev,
                route,
                apdu,
                confirmed,
            });
        }
        out
    }
}

fn nc_destinations<'a>(
    nc: &'a NotificationClass,
    n: &'a EventNotice,
) -> impl Iterator<Item = &'a Destination> {
    nc.recipients
        .iter()
        .filter(|d| d.accepts(n.time, n.transition()))
}

/// Device recipients of every notification class, other than our own.
fn wanted(db: &PointDb) -> impl Iterator<Item = u32> + '_ {
    db.devices
        .iter()
        .flat_map(|d| &d.objects)
        .filter_map(BacObject::as_notification_class)
        .flat_map(|nc| &nc.recipients)
        .filter_map(|d| match d.recipient {
            Recipient::Device(i) if db.device_index(i).is_none() => Some(i),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bask_points::PointValue;
    use chrono::{NaiveDate, NaiveDateTime};

    fn at(m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 2)
            .unwrap()
            .and_hms_opt(9, m, 0)
            .unwrap()
    }

    fn db() -> PointDb {
        let mut db = PointDb::new(2001);
        let mut a = BacObject::new(ObjectType::AnalogInput, 1, "ZN-T", PointValue::Real(70.0));
        a.enable_out_of_range(60.0, 80.0, 1.0, 1, 0);
        let mut b = BacObject::new(
            ObjectType::BinaryInput,
            1,
            "SF-FAIL",
            PointValue::Binary(false),
        );
        b.enable_change_of_state(true, 1, 0);
        db.devices.push(BacDevice {
            instance: 1001,
            name: "AHU-1".into(),
            description: String::new(),
            location: String::new(),
            model_name: String::new(),
            mac: Some(1),
            online: true,
            objects: vec![a, b, BacObject::notification_class(1, "NC")],
            database_revision: 1,
        });
        db.tick(at(0));
        db
    }

    fn ack(object: ObjectId, state: u32, stamp: NaiveDateTime) -> Vec<u8> {
        let mut b = Vec::new();
        ctx_unsigned(&mut b, 0, 1);
        ctx_object_id(&mut b, 1, object);
        ctx_enumerated(&mut b, 2, state);
        open(&mut b, 3);
        time_stamp(&mut b, Some(stamp));
        close(&mut b, 3);
        ctx_char_string(&mut b, 4, "tech");
        open(&mut b, 5);
        time_stamp(&mut b, Some(stamp));
        close(&mut b, 5);
        b
    }

    #[test]
    fn acknowledge_checks_the_time_stamp() {
        let mut db = db();
        db.devices[0].objects[0].present_value = PointValue::Real(90.0);
        db.tick(at(1));
        let ai = ObjectId::new(0, 1);
        assert!(db.devices[0].objects[0].unacknowledged());
        assert_eq!(
            acknowledge(&mut db, 0, &ack(ai, 3, at(2))),
            Ok(Err(BacError::INVALID_TIME_STAMP))
        );
        assert_eq!(
            acknowledge(&mut db, 0, &ack(ObjectId::new(15, 1), 3, at(1))),
            Ok(Err(BacError::NO_ALARM_CONFIGURED))
        );
        db.take_notices();
        assert_eq!(
            acknowledge(&mut db, 0, &ack(ai, 3, at(1))),
            Ok(Ok("ZN-T".into()))
        );
        assert!(!db.devices[0].objects[0].unacknowledged());
        let n = db.take_notices();
        assert_eq!(n[0].notify_type, NotifyType::AckNotification);
        // The ack notification leaves out ack-required, from-state and values.
        let body = notification(&n[0], 0);
        let mut r = Reader::new(&body);
        for tag in [0, 1, 2] {
            r.ctx_raw(tag).unwrap();
        }
        r.bracketed(3).unwrap();
        for tag in [4, 5, 6, 7] {
            r.ctx_raw(tag).unwrap();
        }
        assert_eq!(r.ctx_enumerated(8).unwrap(), 2);
        assert_eq!(r.ctx_enumerated(11).unwrap(), 3);
        assert!(r.at_end());
    }

    #[test]
    fn get_event_information_pages() {
        let mut db = db();
        db.devices[0].objects[0].present_value = PointValue::Real(90.0);
        db.devices[0].objects[1].present_value = PointValue::Binary(true);
        db.tick(at(1));
        let summaries = |reply: Reply| {
            let Reply::ComplexAck(ack) = reply else {
                panic!()
            };
            let mut r = Reader::new(&ack);
            let list = r.bracketed(0).unwrap();
            let more = r.ctx_raw(1).unwrap() == [1];
            let mut ids = Vec::new();
            let mut lr = Reader::new(list);
            while !lr.at_end() {
                ids.push(lr.ctx_object_id(0).unwrap().object_type);
                lr.ctx_raw(1).unwrap();
                lr.ctx_raw(2).unwrap();
                lr.bracketed(3).unwrap();
                lr.ctx_raw(4).unwrap();
                lr.ctx_raw(5).unwrap();
                assert_eq!(app_values(lr.bracketed(6).unwrap()).unwrap().len(), 3);
            }
            (ids, more)
        };
        assert_eq!(
            summaries(get_event_information(&db, 0, &[], 1476).unwrap()),
            (vec![0, 3], false)
        );
        // Room for one at a time.
        assert_eq!(
            summaries(get_event_information(&db, 0, &[], 90).unwrap()),
            (vec![0], true)
        );
        let mut after = Vec::new();
        ctx_object_id(&mut after, 0, ObjectId::new(0, 1));
        assert_eq!(
            summaries(get_event_information(&db, 0, &after, 90).unwrap()),
            (vec![3], false)
        );
    }

    #[test]
    fn routes_to_recipients() {
        let mut db = db();
        let nc = db.devices[0].objects[2]
            .as_notification_class_mut()
            .unwrap();
        let mut device = Destination::local_broadcast();
        device.recipient = Recipient::Device(77);
        device.confirmed = true;
        let mut address = Destination::local_broadcast();
        address.recipient = Recipient::Address {
            network: 0,
            mac: vec![10, 0, 0, 5, 0xBA, 0xC0],
        };
        address.transitions = [false, false, true];
        nc.recipients.extend([device, address]);
        db.devices[0].objects[1].present_value = PointValue::Binary(true);
        db.tick(at(1));
        let n = db.take_notices();
        let mut notifier = Notifier::default();
        // Device 77 isn't bound yet: just the broadcast, and a Who-Is due.
        let out = notifier.route(&db, &n[0]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].route, Route::LocalBroadcast);
        assert_eq!(&out[0].apdu[..2], [0x10, unconfirmed::EVENT_NOTIFICATION]);
        assert_eq!(notifier.unbound(&db, Instant::now()), [77]);
        assert!(notifier.unbound(&db, Instant::now()).is_empty());

        let peer = Peer {
            addr: SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 9), 47808),
            remote: None,
        };
        let mut i_am = Vec::new();
        app_object_id(&mut i_am, ObjectId::new(8, 77));
        notifier.i_am(&db, &i_am, &peer);
        let out = notifier.route(&db, &n[0]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].route, Route::Peer(peer));
        assert!(out[1].confirmed && out[1].apdu[0] == 0x00);

        // Back to normal: the address recipient takes only that.
        db.devices[0].objects[1].present_value = PointValue::Binary(false);
        db.tick(at(2));
        let n = db.take_notices();
        let out = notifier.route(&db, &n[0]);
        assert_eq!(out.len(), 3);
        assert_eq!(
            out[2].route,
            Route::Peer(Peer {
                addr: "10.0.0.5:47808".parse().unwrap(),
                remote: None
            })
        );
    }

    #[test]
    fn class_and_reporting_writes() {
        let mut db = db();
        let nc = &mut db.devices[0].objects[2];
        let mut list = Vec::new();
        let mut d = Destination::local_broadcast();
        d.recipient = Recipient::Device(5);
        destination(&mut list, &d);
        destination(&mut list, &Destination::local_broadcast());
        assert_eq!(
            write_class(nc, RECIPIENT_LIST, None, &list),
            Ok("recipient-list = 2 recipients".into())
        );
        let mut back = Vec::new();
        assert!(read(nc, RECIPIENT_LIST, &mut back));
        assert_eq!(back, list);
        assert!(write_class(nc, PRIORITY, Some(2), &[0x21, 50]).is_ok());
        assert_eq!(nc.as_notification_class().unwrap().priority, [100, 50, 200]);
        assert_eq!(
            write_class(nc, PRIORITY, None, &[0x21, 1, 0x21, 2]),
            Err(BacError::INVALID_DATA_TYPE)
        );
        assert!(write_class(nc, ACK_REQUIRED, None, &[0x82, 0x05, 0xE0]).is_ok());
        assert_eq!(nc.as_notification_class().unwrap().ack_required, [true; 3]);

        let ai = &mut db.devices[0].objects[0];
        assert!(write_reporting(ai, HIGH_LIMIT, &[0x44, 0x42, 0xA0, 0, 0]).is_ok());
        assert!(matches!(
            ai.reporting.as_ref().unwrap().algorithm,
            EventAlgorithm::OutOfRange { high: 80.0, .. }
        ));
        assert_eq!(
            write_reporting(ai, DEADBAND, &[0x44, 0xBF, 0x80, 0, 0]),
            Err(BacError::VALUE_OUT_OF_RANGE)
        );
        assert_eq!(
            write_reporting(ai, ALARM_VALUE, &[0x91, 1]),
            Err(BacError::WRITE_ACCESS_DENIED)
        );
        assert_eq!(
            write_reporting(ai, ACKED_TRANSITIONS, &[0x82, 0x05, 0xE0]),
            Err(BacError::WRITE_ACCESS_DENIED)
        );
        assert!(write_reporting(ai, TIME_DELAY, &[0x21, 30]).is_ok());
        assert_eq!(ai.reporting.as_ref().unwrap().time_delay, 30);
    }
}
