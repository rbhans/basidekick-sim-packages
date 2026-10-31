//! Change-of-value subscriptions (clause 13.1).
//!
//! A BAS subscribes to an object instead of polling it. The device sends the
//! object's present value and status flags once right away, then again
//! whenever the value moves by its cov-increment (analog) or changes at all
//! (binary, multi-state), or the flags change. Subscriptions are keyed the
//! way the standard keys them (subscriber, process id, object), last for
//! their lifetime, and are checked against the database a few times a second.
//!
//! They hold device and object identifiers rather than indexes, so they
//! survive the database being rebuilt and lapse when their object goes away.

use crate::apdu::{self, BacError, confirmed, unconfirmed};
use crate::encoding::*;
use crate::npdu::Peer;
use crate::properties::{self, Target, device_id, encode_value, prop};
use bask_points::{BacDevice, BacObject, ObjectType, PointDb, PointValue};
use std::time::{Duration, Instant};

const MAX_SUBSCRIPTIONS: usize = 4096;

struct Subscription {
    peer: Peer,
    process_id: u32,
    device: u32,
    object: ObjectId,
    confirmed: bool,
    /// None for an indefinite lifetime.
    expires: Option<Instant>,
    /// What the last notification reported.
    last: Option<(PointValue, [bool; 4])>,
}

/// A notification ready to send from device `device` to `peer`.
pub struct Notification {
    pub peer: Peer,
    pub device: usize,
    pub apdu: Vec<u8>,
    pub confirmed: bool,
}

pub enum CovOutcome {
    /// A new or renewed subscription, and its initial notification.
    Subscribed {
        object: String,
        initial: Notification,
    },
    Cancelled,
}

#[derive(Default)]
pub struct CovTable {
    subs: Vec<Subscription>,
    invoke_id: u8,
}

impl CovTable {
    /// Handles SubscribeCOV for device `dev`.
    pub fn subscribe(
        &mut self,
        db: &PointDb,
        dev: usize,
        peer: &Peer,
        body: &[u8],
        now: Instant,
    ) -> DResult<Result<CovOutcome, BacError>> {
        let mut r = Reader::new(body);
        let process_id = u32::try_from(r.ctx_unsigned(0)?).map_err(|_| DecodeError::BadValue)?;
        let id = r.ctx_object_id(1)?;
        let issue_confirmed = r.opt_ctx_bool(2)?;
        let lifetime = r.opt_ctx_unsigned(3)?;
        if !r.at_end() {
            return Err(DecodeError::TooManyArguments);
        }

        let d = &db.devices[dev];
        let id = properties::unwildcard(d, id);
        let object = match properties::resolve(d, id) {
            Ok(Target::Object(i)) => i,
            Ok(Target::Device) => return Ok(Err(BacError::OPTIONAL_FUNCTIONALITY_NOT_SUPPORTED)),
            Err(e) => return Ok(Err(e)),
        };
        let existing = self.subs.iter().position(|s| {
            s.peer == *peer
                && s.process_id == process_id
                && s.device == d.instance
                && s.object == id
        });

        // Neither optional parameter: a cancellation, acknowledged even when
        // there was nothing to cancel.
        if issue_confirmed.is_none() && lifetime.is_none() {
            if let Some(i) = existing {
                self.subs.remove(i);
            }
            return Ok(Ok(CovOutcome::Cancelled));
        }
        let confirmed = issue_confirmed.ok_or(DecodeError::Truncated)?;
        let expires = match lifetime {
            None | Some(0) => None,
            Some(s) => now.checked_add(Duration::from_secs(s)),
        };
        let sub = Subscription {
            peer: peer.clone(),
            process_id,
            device: d.instance,
            object: id,
            confirmed,
            expires,
            last: None,
        };
        let i = match existing {
            Some(i) => {
                self.subs[i] = sub;
                i
            }
            None if self.subs.len() >= MAX_SUBSCRIPTIONS => return Ok(Err(BacError::NO_SPACE)),
            None => {
                self.subs.push(sub);
                self.subs.len() - 1
            }
        };
        let o = &d.objects[object];
        let initial = self.notify(i, d, dev, o, now);
        Ok(Ok(CovOutcome::Subscribed {
            object: o.name.clone(),
            initial,
        }))
    }

    /// Drops expired subscriptions and those whose object is gone, and
    /// returns notifications for objects that changed. Devices that are off
    /// the network (or behind a router that is) stay quiet.
    pub fn tick(&mut self, db: &PointDb, now: Instant) -> Vec<Notification> {
        self.subs
            .retain(|s| s.expires.is_none_or(|e| e > now) && locate(db, s).is_some());
        let router_up = db.local_device().is_some_and(|i| db.devices[i].online);
        let mut out = Vec::new();
        for i in 0..self.subs.len() {
            let Some((dev, object)) = locate(db, &self.subs[i]) else {
                continue;
            };
            let d = &db.devices[dev];
            let o = &d.objects[object];
            if router_up && d.online && changed(self.subs[i].last, o) {
                out.push(self.notify(i, d, dev, o, now));
            }
        }
        out
    }

    fn notify(
        &mut self,
        i: usize,
        d: &BacDevice,
        dev: usize,
        o: &BacObject,
        now: Instant,
    ) -> Notification {
        let s = &mut self.subs[i];
        let value = o.effective();
        let flags = o.status_flags();
        s.last = Some((value, flags));
        let remaining = s.expires.map_or(0, |e| {
            e.saturating_duration_since(now).as_millis().div_ceil(1000) as u64
        });

        let mut b = Vec::new();
        ctx_unsigned(&mut b, 0, s.process_id as u64);
        ctx_object_id(&mut b, 1, device_id(d));
        ctx_object_id(&mut b, 2, s.object);
        ctx_unsigned(&mut b, 3, remaining);
        open(&mut b, 4);
        ctx_enumerated(&mut b, 0, prop::PRESENT_VALUE);
        open(&mut b, 2);
        encode_value(&mut b, o.object_type, value);
        close(&mut b, 2);
        ctx_enumerated(&mut b, 0, prop::STATUS_FLAGS);
        open(&mut b, 2);
        app_bit_string(&mut b, &flags);
        close(&mut b, 2);
        close(&mut b, 4);

        let (peer, confirmed) = (s.peer.clone(), s.confirmed);
        let apdu = if confirmed {
            // Sent once; the ack (or silence) is ignored.
            self.invoke_id = self.invoke_id.wrapping_add(1);
            apdu::confirmed_request(self.invoke_id, confirmed::COV_NOTIFICATION, &b)
        } else {
            apdu::unconfirmed_request(unconfirmed::COV_NOTIFICATION, &b)
        };
        Notification {
            peer,
            device: dev,
            apdu,
            confirmed,
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.subs.len()
    }
}

/// The subscription's device and object indexes in today's database.
fn locate(db: &PointDb, s: &Subscription) -> Option<(usize, usize)> {
    let dev = db.device_index(s.device)?;
    let t = ObjectType::from_bacnet_id(s.object.object_type)?;
    Some((dev, db.devices[dev].find(t, s.object.instance)?))
}

fn changed(last: Option<(PointValue, [bool; 4])>, o: &BacObject) -> bool {
    let Some((value, flags)) = last else {
        return true;
    };
    if flags != o.status_flags() {
        return true;
    }
    match (value, o.effective()) {
        (PointValue::Real(a), PointValue::Real(b)) => a != b && (a - b).abs() >= o.cov_increment,
        (a, b) => a != b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddrV4};

    fn db() -> PointDb {
        let mut db = PointDb::new(2001);
        let mut ai = BacObject::new(ObjectType::AnalogInput, 1, "ZN-T", PointValue::Real(70.0));
        ai.cov_increment = 0.5;
        let bi = BacObject::new(
            ObjectType::BinaryInput,
            1,
            "FAN-S",
            PointValue::Binary(false),
        );
        db.devices.push(BacDevice {
            instance: 1000,
            name: "SITE".into(),
            description: String::new(),
            location: String::new(),
            model_name: String::new(),
            mac: None,
            online: true,
            objects: vec![ai, bi],
            database_revision: 1,
        });
        db
    }

    fn request(object: ObjectId, confirmed: Option<bool>, lifetime: Option<u64>) -> Vec<u8> {
        let mut b = Vec::new();
        ctx_unsigned(&mut b, 0, 7);
        ctx_object_id(&mut b, 1, object);
        if let Some(c) = confirmed {
            ctx_unsigned(&mut b, 2, c as u64); // a context boolean is one octet
        }
        if let Some(l) = lifetime {
            ctx_unsigned(&mut b, 3, l);
        }
        b
    }

    #[test]
    fn notifies_on_increment_and_flag_changes() {
        let mut db = db();
        let peer = Peer {
            addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 47809),
            remote: None,
        };
        let mut cov = CovTable::default();
        let now = Instant::now();
        let ai = ObjectId::new(0, 1);
        let Ok(Ok(CovOutcome::Subscribed { initial, .. })) =
            cov.subscribe(&db, 0, &peer, &request(ai, Some(false), Some(60)), now)
        else {
            panic!()
        };
        assert_eq!(&initial.apdu[..2], [0x10, unconfirmed::COV_NOTIFICATION]);
        // time remaining: 60 s
        assert_eq!(&initial.apdu[2..4], [0x09, 7]);
        assert_eq!(&initial.apdu[14..16], [0x39, 60]);
        assert!(cov.tick(&db, now).is_empty());

        db.devices[0].objects[0].present_value = PointValue::Real(70.3);
        assert!(cov.tick(&db, now).is_empty());
        db.devices[0].objects[0].present_value = PointValue::Real(70.6);
        assert_eq!(cov.tick(&db, now).len(), 1);
        db.devices[0].objects[0].out_of_service = true;
        assert_eq!(cov.tick(&db, now).len(), 1);
        db.devices[0].online = false;
        db.devices[0].objects[0].present_value = PointValue::Real(90.0);
        assert!(cov.tick(&db, now).is_empty());
        db.devices[0].online = true;
        assert_eq!(cov.tick(&db, now).len(), 1);

        // Expiry
        assert!(cov.tick(&db, now + Duration::from_secs(61)).is_empty());
        assert_eq!(cov.len(), 0);
    }

    #[test]
    fn renew_cancel_and_errors() {
        let db = db();
        let peer = Peer {
            addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 47809),
            remote: None,
        };
        let mut cov = CovTable::default();
        let now = Instant::now();
        let bi = ObjectId::new(3, 1);
        let Ok(Ok(CovOutcome::Subscribed { initial, .. })) =
            cov.subscribe(&db, 0, &peer, &request(bi, Some(true), None), now)
        else {
            panic!()
        };
        assert_eq!(initial.apdu[0], 0x00);
        assert_eq!(initial.apdu[3], confirmed::COV_NOTIFICATION);
        assert!(matches!(
            cov.subscribe(&db, 0, &peer, &request(bi, Some(true), Some(0)), now),
            Ok(Ok(_))
        ));
        assert_eq!(cov.len(), 1);
        assert!(matches!(
            cov.subscribe(&db, 0, &peer, &request(bi, None, None), now),
            Ok(Ok(CovOutcome::Cancelled))
        ));
        assert_eq!(cov.len(), 0);
        assert!(matches!(
            cov.subscribe(
                &db,
                0,
                &peer,
                &request(ObjectId::new(3, 9), Some(true), None),
                now
            ),
            Ok(Err(BacError::UNKNOWN_OBJECT))
        ));
        assert!(matches!(
            cov.subscribe(
                &db,
                0,
                &peer,
                &request(ObjectId::new(8, 1000), Some(true), None),
                now
            ),
            Ok(Err(BacError::OPTIONAL_FUNCTIONALITY_NOT_SUPPORTED))
        ));
        assert!(matches!(
            cov.subscribe(&db, 0, &peer, &request(bi, None, Some(60)), now),
            Err(DecodeError::Truncated)
        ));
    }
}
