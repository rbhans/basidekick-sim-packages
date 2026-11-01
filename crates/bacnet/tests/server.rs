//! The server as a BAS sees it: raw BACnet/IP packets over loopback.
//!
//! Requests are built by hand and replies picked apart with a minimal tag
//! walker, independent of the crate's own encoder.

use bask_bacnet::{BacnetServer, ServerConfig, ServerEvent};
use bask_points::{BacDevice, BacObject, ObjectType, PointDb, PointValue, SharedDb, Units};
use chrono::{Duration as SimDuration, NaiveDate, NaiveDateTime, NaiveTime};
use parking_lot::Mutex;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const NET: u16 = 2001;
const SITE: u32 = 1000;
const VAV: u32 = 1001;
const AHU: u32 = 1002;

const AI: u16 = 0;
const AO: u16 = 1;
const AV: u16 = 2;
const BI: u16 = 3;
const DEVICE: u16 = 8;
const NC: u16 = 15;
const SCH: u16 = 17;
const MSV: u16 = 19;
const TL: u16 = 20;

const EXCEPTION_SCHEDULE: u32 = 38;
const LOCAL_DATE: u32 = 56;
const OUT_OF_SERVICE: u32 = 81;
const PRESENT_VALUE: u32 = 85;
const PRIORITY_ARRAY: u32 = 87;
const RECIPIENT_LIST: u32 = 102;
const WEEKLY_SCHEDULE: u32 = 123;
const LOG_BUFFER: u32 = 131;
const RECORD_COUNT: u32 = 141;
const DATABASE_REVISION: u32 = 155;
const CURRENT_COMMAND_PRIORITY: u32 = 431;

fn building() -> SharedDb {
    let device = |instance, name: &str, mac, objects| BacDevice {
        instance,
        name: name.into(),
        description: String::new(),
        location: "Lab".into(),
        model_name: "Sim".into(),
        mac,
        online: true,
        objects,
        database_revision: 1,
    };
    let mut oat = BacObject::new(ObjectType::AnalogInput, 1, "OA-T", PointValue::Real(55.0));
    oat.units = Units::DegreesFahrenheit;

    let mut zn_t = BacObject::new(ObjectType::AnalogInput, 1, "ZN-T", PointValue::Real(72.0));
    zn_t.units = Units::DegreesFahrenheit;
    let mut zn_sp = BacObject::new(ObjectType::AnalogValue, 1, "ZN-SP", PointValue::Real(70.0));
    zn_sp.commandable = true;
    zn_sp.units = Units::DegreesFahrenheit;
    zn_sp.min_value = Some(55.0);
    zn_sp.max_value = Some(85.0);
    let mut dmpr = BacObject::new(
        ObjectType::AnalogOutput,
        1,
        "DMPR-POS",
        PointValue::Real(30.0),
    );
    dmpr.units = Units::Percent;
    let mut occ = BacObject::new(
        ObjectType::MultiStateValue,
        1,
        "OCC-MODE",
        PointValue::Multi(1),
    );
    occ.commandable = true;
    occ.states = vec!["Auto".into(), "Occupied".into(), "Unoccupied".into()];

    let mut sa_t = BacObject::new(ObjectType::AnalogInput, 1, "SA-T", PointValue::Real(55.0));
    sa_t.units = Units::DegreesFahrenheit;
    let sf_c = BacObject::new(
        ObjectType::BinaryOutput,
        1,
        "SF-C",
        PointValue::Binary(true),
    );
    let mut sf_fail = BacObject::new(
        ObjectType::BinaryInput,
        1,
        "SF-FAIL",
        PointValue::Binary(false),
    );
    sf_fail.enable_change_of_state(true, 1, 0);
    let hm = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
    let day = vec![(hm(7, 0), true), (hm(17, 0), false)];
    let weekly = std::array::from_fn(|d| if d < 5 { day.clone() } else { vec![] });

    let mut db = PointDb::new(NET);
    db.devices = vec![
        device(SITE, "SITE", None, vec![oat]),
        device(VAV, "VAV-1", Some(1), vec![zn_t, zn_sp, dmpr, occ]),
        device(
            AHU,
            "AHU-1",
            Some(2),
            vec![
                sa_t,
                sf_c,
                sf_fail,
                BacObject::schedule(1, "OCC-SCHED", weekly),
                BacObject::trend_log(1, "SA-T-TL", (ObjectType::AnalogInput, 1), 60, 500),
                BacObject::notification_class(1, "ALARMS"),
            ],
        ),
    ];
    Arc::new(Mutex::new(db))
}

// ---------------------------------------------------------------------------
// A raw client

struct Client {
    _server: BacnetServer,
    server_addr: SocketAddr,
    db: SharedDb,
    events: Receiver<ServerEvent>,
    sock: UdpSocket,
    invoke: u8,
}

/// A received packet, split up.
#[derive(Debug)]
struct Packet {
    dnet: Option<NetAddr>,
    snet: Option<NetAddr>,
    hop_count: Option<u8>,
    message: Option<u8>,
    payload: Vec<u8>,
}

fn start() -> Client {
    let db = building();
    let (tx, events) = mpsc::channel();
    let config = ServerConfig {
        bind_ip: Ipv4Addr::LOCALHOST,
        port: 0,
        broadcast: None,
    };
    let server = BacnetServer::start(config, db.clone(), tx).unwrap();
    let server_addr = server.local_addr();
    assert!(
        matches!(events.recv().unwrap(), ServerEvent::Listening { addr } if addr == server_addr)
    );
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    Client {
        _server: server,
        server_addr,
        db,
        events,
        sock,
        invoke: 0,
    }
}

impl Client {
    fn send_raw(&self, packet: &[u8]) {
        self.sock.send_to(packet, self.server_addr).unwrap();
    }

    /// Sends an NPDU (Original-Unicast) with optional DNET/DADR and SNET/SADR.
    fn send(
        &self,
        dest: Option<(u16, &[u8])>,
        source: Option<(u16, &[u8])>,
        message: Option<u8>,
        payload: &[u8],
    ) {
        let mut ctrl = 0u8;
        if message.is_some() {
            ctrl |= 0x80;
        }
        if dest.is_some() {
            ctrl |= 0x20;
        }
        if source.is_some() {
            ctrl |= 0x08;
        }
        let mut n = vec![0x01, ctrl];
        for (net, mac) in [dest, source].into_iter().flatten() {
            n.extend_from_slice(&net.to_be_bytes());
            n.push(mac.len() as u8);
            n.extend_from_slice(mac);
        }
        if dest.is_some() {
            n.push(255);
        }
        n.extend(message);
        n.extend_from_slice(payload);
        let mut p = vec![0x81, 0x0A];
        p.extend_from_slice(&((n.len() + 4) as u16).to_be_bytes());
        p.extend(n);
        self.send_raw(&p);
    }

    /// Everything that arrives within `wait`.
    fn recv_for(&self, wait: Duration) -> Vec<Packet> {
        let end = Instant::now() + wait;
        let mut got = Vec::new();
        let mut buf = [0u8; 2048];
        while Instant::now() < end {
            if let Ok((n, _)) = self.sock.recv_from(&mut buf) {
                got.push(parse(&buf[..n]));
            }
        }
        got
    }

    /// The first packet within a second.
    fn recv(&self) -> Option<Packet> {
        let end = Instant::now() + Duration::from_secs(1);
        let mut buf = [0u8; 2048];
        while Instant::now() < end {
            if let Ok((n, _)) = self.sock.recv_from(&mut buf) {
                return Some(parse(&buf[..n]));
            }
        }
        None
    }

    /// Sends a confirmed request to the device at `mac` (None: the site
    /// controller) and returns the reply APDU and its NPDU.
    fn request(&mut self, mac: Option<u8>, service: u8, body: &[u8]) -> Packet {
        self.request_with(mac, 0x05, service, body)
            .expect("no reply")
    }

    fn request_with(
        &mut self,
        mac: Option<u8>,
        max_apdu: u8,
        service: u8,
        body: &[u8],
    ) -> Option<Packet> {
        self.invoke = self.invoke.wrapping_add(1);
        let mut apdu = vec![0x00, max_apdu, self.invoke, service];
        apdu.extend_from_slice(body);
        let mac_bytes = mac.map(|m| [m]);
        self.send(mac_bytes.as_ref().map(|m| (NET, &m[..])), None, None, &apdu);
        let reply = self.recv()?;
        assert_eq!(reply.payload[1], self.invoke, "invoke id");
        Some(reply)
    }

    fn read(
        &mut self,
        mac: Option<u8>,
        object: (u16, u32),
        property: u32,
        index: Option<u8>,
    ) -> Packet {
        let mut body = ctx_oid(0, object);
        body.extend(ctx_enum(1, property));
        if let Some(i) = index {
            body.extend([0x29, i]);
        }
        self.request(mac, 12, &body)
    }

    /// The value tags of a ReadProperty-ACK.
    fn read_value(
        &mut self,
        mac: Option<u8>,
        object: (u16, u32),
        property: u32,
        index: Option<u8>,
    ) -> Vec<T> {
        let reply = self.read(mac, object, property, index);
        assert_eq!(
            &reply.payload[..1],
            [0x30],
            "not a ComplexAck: {:02X?}",
            reply.payload
        );
        let tags = tags(&reply.payload[3..]);
        let open = tags.iter().position(|t| *t == T::Open(3)).unwrap();
        assert_eq!(tags.last(), Some(&T::Close(3)));
        tags[open + 1..tags.len() - 1].to_vec()
    }

    fn write(
        &mut self,
        mac: Option<u8>,
        object: (u16, u32),
        property: u32,
        value: &[u8],
        priority: Option<u8>,
    ) -> Packet {
        self.write_at(mac, object, property, None, value, priority)
    }

    fn write_at(
        &mut self,
        mac: Option<u8>,
        object: (u16, u32),
        property: u32,
        index: Option<u8>,
        value: &[u8],
        priority: Option<u8>,
    ) -> Packet {
        let mut body = ctx_oid(0, object);
        body.extend(ctx_enum(1, property));
        if let Some(i) = index {
            body.extend([0x29, i]);
        }
        body.push(0x3E);
        body.extend_from_slice(value);
        body.push(0x3F);
        if let Some(p) = priority {
            body.extend([0x49, p]);
        }
        self.request(mac, 15, &body)
    }

    fn events(&self) -> Vec<ServerEvent> {
        self.events.try_iter().collect()
    }
}

fn parse(p: &[u8]) -> Packet {
    assert_eq!(p[0], 0x81);
    assert_eq!(p[1], 0x0A, "replies are unicast");
    assert_eq!(u16::from_be_bytes([p[2], p[3]]) as usize, p.len());
    let n = &p[4..];
    assert_eq!(n[0], 0x01);
    let ctrl = n[1];
    let mut i = 2;
    let mut addr = || {
        let net = u16::from_be_bytes([n[i], n[i + 1]]);
        let len = n[i + 2] as usize;
        let mac = n[i + 3..i + 3 + len].to_vec();
        i += 3 + len;
        (net, mac)
    };
    let dnet = (ctrl & 0x20 != 0).then(&mut addr);
    let snet = (ctrl & 0x08 != 0).then(&mut addr);
    let hop_count = dnet.is_some().then(|| {
        i += 1;
        n[i - 1]
    });
    let message = (ctrl & 0x80 != 0).then(|| {
        i += 1;
        n[i - 1]
    });
    Packet {
        dnet,
        snet,
        hop_count,
        message,
        payload: n[i..].to_vec(),
    }
}

fn ctx_oid(tag: u8, (t, instance): (u16, u32)) -> Vec<u8> {
    let v = (t as u32) << 22 | instance;
    let mut b = vec![tag << 4 | 0x0C];
    b.extend_from_slice(&v.to_be_bytes());
    b
}

/// A context-tagged ENUMERATED in one or two octets.
fn ctx_enum(tag: u8, v: u32) -> Vec<u8> {
    if v < 256 {
        vec![tag << 4 | 0x09, v as u8]
    } else {
        vec![tag << 4 | 0x0A, (v >> 8) as u8, v as u8]
    }
}

fn real(v: f32) -> Vec<u8> {
    let mut b = vec![0x44];
    b.extend_from_slice(&v.to_be_bytes());
    b
}

/// A decoded tag: application tags keep their number and content.
#[derive(Clone, Debug, PartialEq)]
enum T {
    App(u8, Vec<u8>),
    Ctx(u8, Vec<u8>),
    Open(u8),
    Close(u8),
}

fn tags(mut b: &[u8]) -> Vec<T> {
    let mut out = Vec::new();
    while !b.is_empty() {
        let first = b[0];
        b = &b[1..];
        let mut number = first >> 4;
        if number == 0x0F {
            number = b[0];
            b = &b[1..];
        }
        let context = first & 0x08 != 0;
        let lvt = first & 0x07;
        if context && lvt >= 6 {
            out.push(if lvt == 6 {
                T::Open(number)
            } else {
                T::Close(number)
            });
            continue;
        }
        if !context && number == 1 {
            out.push(T::App(1, vec![lvt]));
            continue;
        }
        let mut len = lvt as usize;
        if lvt == 5 {
            len = b[0] as usize;
            b = &b[1..];
            if len == 254 {
                len = u16::from_be_bytes([b[0], b[1]]) as usize;
                b = &b[2..];
            }
        }
        let content = b[..len].to_vec();
        b = &b[len..];
        out.push(if context {
            T::Ctx(number, content)
        } else {
            T::App(number, content)
        });
    }
    out
}

fn as_real(t: &T) -> f32 {
    match t {
        T::App(4, c) => f32::from_be_bytes(c[..].try_into().unwrap()),
        other => panic!("not a REAL: {other:?}"),
    }
}

fn as_unsigned(t: &T) -> u64 {
    match t {
        T::App(2, c) => c.iter().fold(0, |acc, &b| acc << 8 | b as u64),
        other => panic!("not an UNSIGNED: {other:?}"),
    }
}

fn as_string(t: &T) -> String {
    match t {
        T::App(7, c) if c[0] == 0 => String::from_utf8(c[1..].to_vec()).unwrap(),
        other => panic!("not a UTF-8 CharacterString: {other:?}"),
    }
}

fn instance_of(t: &T) -> (u16, u32) {
    let (T::App(12, c) | T::Ctx(_, c)) = t else {
        panic!("not an object id: {t:?}")
    };
    let v = u32::from_be_bytes(c[..].try_into().unwrap());
    ((v >> 22) as u16, v & 0x3F_FFFF)
}

/// An Error PDU's class and code.
fn error_of(p: &Packet) -> (u8, u8) {
    assert_eq!(p.payload[0], 0x50, "not an Error: {:02X?}", p.payload);
    assert_eq!(p.payload[3], 0x91);
    assert_eq!(p.payload[5], 0x91);
    (p.payload[4], p.payload[6])
}

/// A network number and MAC.
type NetAddr = (u16, Vec<u8>);

/// I-Am replies by device instance: (instance, SNET/SADR, tags).
fn i_ams(packets: &[Packet]) -> Vec<(u32, Option<NetAddr>, Vec<T>)> {
    let mut v: Vec<_> = packets
        .iter()
        .filter(|p| p.payload.starts_with(&[0x10, 0x00]))
        .map(|p| {
            let t = tags(&p.payload[2..]);
            (instance_of(&t[0]).1, p.snet.clone(), t)
        })
        .collect();
    v.sort_by_key(|(i, ..)| *i);
    v
}

// ---------------------------------------------------------------------------

#[test]
fn who_is_finds_site_and_routed_devices() {
    let c = start();
    c.send(None, None, None, &[0x10, 0x08]);
    let found = i_ams(&c.recv_for(Duration::from_millis(400)));
    assert_eq!(
        found.iter().map(|(i, ..)| *i).collect::<Vec<_>>(),
        [SITE, VAV, AHU]
    );
    assert_eq!(found[0].1, None);
    assert_eq!(found[1].1, Some((NET, vec![1])));
    assert_eq!(found[2].1, Some((NET, vec![2])));
    let t = &found[1].2;
    assert_eq!(instance_of(&t[0]), (DEVICE, VAV));
    assert_eq!(as_unsigned(&t[1]), 1476);
    assert_eq!(t[2], T::App(9, vec![3])); // no segmentation
    assert_eq!(as_unsigned(&t[3]), 999);
    assert!(
        c.events()
            .iter()
            .any(|e| matches!(e, ServerEvent::WhoIs { .. }))
    );

    // With a range
    c.send(
        None,
        None,
        None,
        &[0x10, 0x08, 0x0A, 0x03, 0xE9, 0x1A, 0x03, 0xE9],
    );
    let found = i_ams(&c.recv_for(Duration::from_millis(300)));
    assert_eq!(found.iter().map(|(i, ..)| *i).collect::<Vec<_>>(), [VAV]);

    // Broadcast on the virtual network reaches only routed devices.
    c.send(Some((NET, &[])), None, None, &[0x10, 0x08]);
    let found = i_ams(&c.recv_for(Duration::from_millis(300)));
    assert_eq!(
        found.iter().map(|(i, ..)| *i).collect::<Vec<_>>(),
        [VAV, AHU]
    );

    // A global broadcast reaches all of them.
    c.send(Some((0xFFFF, &[])), None, None, &[0x10, 0x08]);
    assert_eq!(i_ams(&c.recv_for(Duration::from_millis(300))).len(), 3);
}

#[test]
fn read_property_routes_by_dadr() {
    let mut c = start();
    let reply = c.read(Some(1), (AI, 1), PRESENT_VALUE, None);
    assert_eq!(reply.snet, Some((NET, vec![1])));
    assert_eq!(reply.dnet, None);
    assert_eq!(&reply.payload[..3], [0x30, c.invoke, 12]);
    let t = tags(&reply.payload[3..]);
    assert_eq!(instance_of(&t[0]), (AI, 1));
    assert_eq!(t[1], T::Ctx(1, vec![PRESENT_VALUE as u8]));
    assert_eq!(as_real(&t[3]), 72.0);

    // The site controller's own AI: same object id, no SNET.
    let reply = c.read(None, (AI, 1), PRESENT_VALUE, None);
    assert_eq!(reply.snet, None);
    assert_eq!(as_real(&tags(&reply.payload[3..])[3]), 55.0);

    // The wildcard device instance means the addressed device.
    let v = c.read_value(Some(2), (DEVICE, 4_194_303), 77, None);
    assert_eq!(as_string(&v[0]), "AHU-1");
    let v = c.read_value(None, (DEVICE, SITE), 76, None);
    assert_eq!(
        v.iter().map(instance_of).collect::<Vec<_>>(),
        [(DEVICE, SITE), (AI, 1)]
    );
    let v = c.read_value(Some(1), (DEVICE, VAV), 76, Some(0));
    assert_eq!(as_unsigned(&v[0]), 5);

    // A request that came through another router goes back through it.
    let mut rp = vec![0x00, 0x05, 0x77, 12];
    rp.extend(ctx_oid(0, (AV, 1)));
    rp.extend(ctx_enum(1, PRESENT_VALUE));
    c.send(Some((NET, &[1])), Some((7, &[0x0A, 0x0B])), None, &rp);
    let reply = c.recv().unwrap();
    assert_eq!(reply.dnet, Some((7, vec![0x0A, 0x0B])));
    assert_eq!(reply.snet, Some((NET, vec![1])));
    assert_eq!(reply.hop_count, Some(255));
    assert_eq!(&reply.payload[..3], [0x30, 0x77, 12]);

    // No device at that MAC: silence, like a real router's dead trunk.
    c.send(Some((NET, &[9])), None, None, &rp);
    assert!(c.recv_for(Duration::from_millis(300)).is_empty());

    // A network we don't route to.
    c.send(Some((9, &[1])), None, None, &rp);
    let reply = c.recv().unwrap();
    assert_eq!(reply.message, Some(0x03));
    assert_eq!(reply.payload, [1, 0, 9]);

    let stats = c._server.stats();
    assert!(stats.requests >= 6);
    assert!(stats.last_client.is_some() && stats.last_request.is_some());
}

#[test]
fn rpm_all_on_a_multistate_value() {
    let mut c = start();
    let mut body = ctx_oid(0, (MSV, 1));
    body.extend([0x1E, 0x09, 8, 0x1F]); // ALL
    let reply = c.request(Some(1), 14, &body);
    assert_eq!(&reply.payload[..3], [0x30, c.invoke, 14]);
    assert_eq!(reply.snet, Some((NET, vec![1])));
    let t = tags(&reply.payload[3..]);
    assert_eq!(instance_of(&t[0]), (MSV, 1));
    // property id → value tags
    let mut props = Vec::new();
    let mut i = 2;
    while let T::Ctx(2, p) = &t[i] {
        let p = p.iter().fold(0u32, |acc, &b| acc << 8 | b as u32);
        assert_eq!(t[i + 1], T::Open(4), "property {p} failed");
        let end = i + 1 + t[i + 1..].iter().position(|x| *x == T::Close(4)).unwrap();
        props.push((p, t[i + 2..end].to_vec()));
        i = end + 1;
    }
    assert_eq!(t[i], T::Close(1));
    let get = |id: u32| {
        &props
            .iter()
            .find(|(p, _)| *p == id)
            .unwrap_or_else(|| panic!("no property {id}"))
            .1
    };
    assert_eq!(as_string(&get(77)[0]), "OCC-MODE");
    assert_eq!(as_unsigned(&get(85)[0]), 1);
    assert_eq!(as_unsigned(&get(74)[0]), 3);
    assert_eq!(
        get(110).iter().map(as_string).collect::<Vec<_>>(),
        ["Auto", "Occupied", "Unoccupied"]
    );
    assert_eq!(get(87).len(), 16);
    assert!(get(87).iter().all(|v| *v == T::App(0, vec![])));
    assert_eq!(as_unsigned(&get(104)[0]), 1);
    assert_eq!(get(431)[0], T::App(0, vec![])); // current-command-priority: NULL
    assert_eq!(get(111)[0], T::App(8, vec![0x04, 0x00])); // status-flags
    assert!(get(371).len() > 8); // property-list
}

#[test]
fn write_then_relinquish() {
    let mut c = start();
    let reply = c.write(Some(1), (AV, 1), PRESENT_VALUE, &real(74.0), Some(8));
    assert_eq!(reply.payload, [0x20, c.invoke, 15]);
    assert_eq!(reply.snet, Some((NET, vec![1])));

    let slots = c.read_value(Some(1), (AV, 1), PRIORITY_ARRAY, None);
    assert_eq!(slots.len(), 16);
    for (i, s) in slots.iter().enumerate() {
        if i == 7 {
            assert_eq!(as_real(s), 74.0);
        } else {
            assert_eq!(*s, T::App(0, vec![]), "slot {}", i + 1);
        }
    }
    assert_eq!(
        as_unsigned(&c.read_value(Some(1), (AV, 1), PRIORITY_ARRAY, Some(0))[0]),
        16
    );
    assert_eq!(
        as_real(&c.read_value(Some(1), (AV, 1), PRIORITY_ARRAY, Some(8))[0]),
        74.0
    );
    assert_eq!(
        as_real(&c.read_value(Some(1), (AV, 1), PRESENT_VALUE, None)[0]),
        74.0
    );
    assert_eq!(
        as_unsigned(&c.read_value(Some(1), (AV, 1), CURRENT_COMMAND_PRIORITY, None)[0]),
        8
    );

    // NULL relinquishes
    let reply = c.write(Some(1), (AV, 1), PRESENT_VALUE, &[0x00], Some(8));
    assert_eq!(reply.payload, [0x20, c.invoke, 15]);
    assert_eq!(
        as_real(&c.read_value(Some(1), (AV, 1), PRESENT_VALUE, None)[0]),
        70.0
    );
    assert!(
        c.db.lock().devices[1].objects[1]
            .priority_array
            .iter()
            .all(Option::is_none)
    );

    // Above max-pres-value
    let reply = c.write(Some(1), (AV, 1), PRESENT_VALUE, &real(99.0), Some(8));
    assert_eq!(error_of(&reply), (2, 37));

    let writes: Vec<_> = c
        .events()
        .into_iter()
        .filter_map(|e| match e {
            ServerEvent::Write {
                device,
                object,
                text,
                ..
            } => Some(format!("{device} {object}: {text}")),
            _ => None,
        })
        .collect();
    assert_eq!(
        writes,
        [
            "VAV-1 ZN-SP: present-value = 74.0 °F @ priority 8",
            "VAV-1 ZN-SP: relinquished priority 8"
        ]
    );
    assert_eq!(c._server.stats().writes, 2);
}

#[test]
fn errors() {
    let mut c = start();
    // An AI in service isn't writable.
    let reply = c.write(Some(1), (AI, 1), PRESENT_VALUE, &real(60.0), None);
    assert_eq!(error_of(&reply), (2, 40));
    // ... until it's out of service.
    assert_eq!(
        c.write(Some(1), (AI, 1), 81, &[0x11], None).payload[0],
        0x20
    );
    assert_eq!(
        c.write(Some(1), (AI, 1), PRESENT_VALUE, &real(60.0), None)
            .payload[0],
        0x20
    );
    // The wrong type for an AO
    assert_eq!(
        error_of(&c.write(Some(1), (AO, 1), PRESENT_VALUE, &[0x91, 0x01], Some(8))),
        (2, 9)
    );
    // Unknown object, unknown property, index on a scalar, index out of range
    assert_eq!(
        error_of(&c.read(Some(1), (AI, 99), PRESENT_VALUE, None)),
        (1, 31)
    );
    assert_eq!(
        error_of(&c.read(Some(1), (AI, 1), PRIORITY_ARRAY, None)),
        (2, 32)
    );
    assert_eq!(
        error_of(&c.read(Some(1), (AI, 1), PRESENT_VALUE, Some(1))),
        (2, 50)
    );
    assert_eq!(
        error_of(&c.read(Some(1), (AV, 1), PRIORITY_ARRAY, Some(17))),
        (2, 42)
    );
    assert_eq!(
        error_of(&c.read(Some(1), (DEVICE, SITE), 77, None)),
        (1, 31)
    );

    // Unrecognized service (AtomicReadFile) → Reject 9; malformed → Reject 5.
    assert_eq!(
        c.request(None, 6, &ctx_oid(0, (AI, 1))).payload,
        [0x60, c.invoke, 9]
    );
    assert_eq!(
        c.request(None, 26, &ctx_oid(0, (AI, 1))).payload,
        [0x60, c.invoke, 5]
    );
    assert_eq!(
        c.request(None, 12, &ctx_oid(0, (AI, 1))).payload,
        [0x60, c.invoke, 5]
    );

    // Too big for a 50-byte max APDU → Abort, segmentation-not-supported.
    let mut body = ctx_oid(0, (DEVICE, SITE));
    body.extend([0x1E, 0x09, 8, 0x1F]);
    let reply = c.request_with(None, 0x00, 14, &body).unwrap();
    assert_eq!(reply.payload, [0x71, c.invoke, 4]);

    // BBMD functions get a NAK.
    c.send_raw(&[0x81, 0x05, 0x00, 0x06, 0x00, 0x3C]);
    let mut buf = [0u8; 64];
    let (n, _) = {
        c.sock
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        c.sock.recv_from(&mut buf).unwrap()
    };
    assert_eq!(&buf[..n], [0x81, 0x00, 0x00, 0x06, 0x00, 0x30]);
}

#[test]
fn offline_devices_are_silent() {
    let mut c = start();
    c.db.lock().devices[2].online = false;
    let mut rp = vec![0x00, 0x05, 0x01, 12];
    rp.extend(ctx_oid(0, (AI, 1)));
    rp.extend(ctx_enum(1, PRESENT_VALUE));
    c.send(Some((NET, &[2])), None, None, &rp);
    assert!(c.recv_for(Duration::from_millis(500)).is_empty());
    c.send(None, None, None, &[0x10, 0x08]);
    assert_eq!(i_ams(&c.recv_for(Duration::from_millis(300))).len(), 2);

    // With the router down, nothing behind it answers either.
    c.db.lock().devices[0].online = false;
    c.send(None, None, None, &[0x10, 0x08]);
    c.send(Some((NET, &[1])), None, None, &rp);
    c.send(None, None, Some(0x00), &[]);
    assert!(c.recv_for(Duration::from_millis(500)).is_empty());

    c.db.lock().devices[0].online = true;
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        as_real(&c.read_value(Some(1), (AI, 1), PRESENT_VALUE, None)[0]),
        72.0
    );
}

#[test]
fn who_is_router_to_network() {
    let c = start();
    for asked in [&[][..], &[0x07, 0xD1]] {
        c.send(None, None, Some(0x00), asked);
        let reply = c.recv().unwrap();
        assert_eq!(reply.message, Some(0x01));
        assert_eq!(reply.payload, [0x07, 0xD1]);
    }
    c.send(None, None, Some(0x00), &[0x00, 0x09]);
    assert!(c.recv_for(Duration::from_millis(300)).is_empty());
}

#[test]
fn cov_subscription_notifies_changes() {
    let mut c = start();
    // process 5, ZN-T, unconfirmed, 60 s
    let mut body = vec![0x09, 5];
    body.extend(ctx_oid(1, (AI, 1)));
    body.extend([0x29, 0x00, 0x39, 60]);
    let ack = c.request(Some(1), 5, &body);
    assert_eq!(ack.payload, [0x20, c.invoke, 5]);

    let check = |p: Packet, value: f32| {
        assert_eq!(p.snet, Some((NET, vec![1])));
        assert_eq!(&p.payload[..2], [0x10, 0x02]);
        let t = tags(&p.payload[2..]);
        assert_eq!(t[0], T::Ctx(0, vec![5]));
        assert_eq!(instance_of(&t[1]), (DEVICE, VAV));
        assert_eq!(instance_of(&t[2]), (AI, 1));
        let T::Ctx(3, remaining) = &t[3] else {
            panic!()
        };
        assert!((55..=60).contains(&remaining[0]));
        assert_eq!(t[4], T::Open(4));
        assert_eq!(t[5], T::Ctx(0, vec![PRESENT_VALUE as u8]));
        assert_eq!(as_real(&t[7]), value);
        assert_eq!(t[9], T::Ctx(0, vec![111]));
        assert_eq!(t[11], T::App(8, vec![0x04, 0x00]));
    };
    check(c.recv().expect("initial notification"), 72.0);
    assert!(c.recv_for(Duration::from_millis(400)).is_empty());

    c.db.lock().devices[1].objects[0].present_value = PointValue::Real(73.5);
    check(c.recv().expect("change notification"), 73.5);
    assert!(
        c.events().iter().any(
            |e| matches!(e, ServerEvent::Subscribed { device, object, .. } if device == "VAV-1" && object == "ZN-T")
        )
    );

    // Cancel: no more notifications.
    let mut cancel = vec![0x09, 5];
    cancel.extend(ctx_oid(1, (AI, 1)));
    assert_eq!(c.request(Some(1), 5, &cancel).payload, [0x20, c.invoke, 5]);
    c.db.lock().devices[1].objects[0].present_value = PointValue::Real(80.0);
    assert!(c.recv_for(Duration::from_millis(500)).is_empty());
}

// ---------------------------------------------------------------------------
// Schedules, trend logs and alarms (all on AHU-1, MAC 2)

/// 2026-11-02 is a Monday.
fn monday(h: u32, m: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 11, 2)
        .unwrap()
        .and_hms_opt(h, m, 0)
        .unwrap()
}

fn ack(c: &Client, p: &Packet) {
    assert_eq!(p.payload, [0x20, c.invoke, 15], "not a SimpleAck");
}

/// A write to AHU-1 that has to take.
fn write_ok(c: &mut Client, object: (u16, u32), property: u32, value: &[u8]) {
    let reply = c.write(Some(2), object, property, value, None);
    ack(c, &reply);
}

#[test]
fn schedule_reads_and_writes() {
    let mut c = start();
    c.db.lock().tick(monday(8, 0));
    let sched = (SCH, 1);
    assert_eq!(
        c.read_value(Some(2), sched, PRESENT_VALUE, None),
        [T::App(9, vec![1])]
    );
    assert_eq!(
        as_unsigned(&c.read_value(Some(2), sched, WEEKLY_SCHEDULE, Some(0))[0]),
        7
    );
    assert_eq!(
        c.read_value(Some(2), sched, WEEKLY_SCHEDULE, Some(1)),
        [
            T::Open(0),
            T::App(11, vec![7, 0, 0, 0]),
            T::App(9, vec![1]),
            T::App(11, vec![17, 0, 0, 0]),
            T::App(9, vec![0]),
            T::Close(0)
        ]
    );
    assert_eq!(
        c.read_value(Some(2), sched, WEEKLY_SCHEDULE, None)
            .iter()
            .filter(|t| **t == T::Open(0))
            .count(),
        7
    );
    // The device's clock is the sim's.
    assert_eq!(
        c.read_value(Some(2), (DEVICE, AHU), LOCAL_DATE, None),
        [T::App(10, vec![126, 11, 2, 1])]
    );
    let revision = as_unsigned(&c.read_value(Some(2), (DEVICE, AHU), DATABASE_REVISION, None)[0]);

    // Mondays become 06:30–18:00.
    let mon = [
        0x0E, 0xB4, 6, 30, 0, 0, 0x91, 1, 0xB4, 18, 0, 0, 0, 0x91, 0, 0x0F,
    ];
    let reply = c.write_at(Some(2), sched, WEEKLY_SCHEDULE, Some(1), &mon, None);
    ack(&c, &reply);
    let back: Vec<u8> = c
        .read(Some(2), sched, WEEKLY_SCHEDULE, Some(1))
        .payload
        .to_vec();
    assert!(back.windows(mon.len()).any(|w| w == mon));
    assert_eq!(
        as_unsigned(&c.read_value(Some(2), (DEVICE, AHU), DATABASE_REVISION, None)[0]),
        revision + 1
    );
    // Not a BinaryPV; the size of a fixed array.
    let two = [0x0E, 0xB4, 6, 30, 0, 0, 0x91, 2, 0x0F];
    assert_eq!(
        error_of(&c.write_at(Some(2), sched, WEEKLY_SCHEDULE, Some(2), &two, None)),
        (2, 37)
    );
    assert_eq!(
        error_of(&c.write_at(Some(2), sched, WEEKLY_SCHEDULE, Some(0), &[0x21, 6], None)),
        (2, 40)
    );

    // In service the schedule owns its present value; out of service, we do.
    assert_eq!(
        error_of(&c.write(Some(2), sched, PRESENT_VALUE, &[0x91, 0], None)),
        (2, 40)
    );
    write_ok(&mut c, sched, OUT_OF_SERVICE, &[0x11]);
    write_ok(&mut c, sched, PRESENT_VALUE, &[0x91, 0]);
    assert_eq!(
        c.read_value(Some(2), sched, PRESENT_VALUE, None),
        [T::App(9, vec![0])]
    );
    write_ok(&mut c, sched, OUT_OF_SERVICE, &[0x10]);
    assert_eq!(
        c.read_value(Some(2), sched, PRESENT_VALUE, None),
        [T::App(9, vec![1])]
    );

    // A holiday today, off from midnight: it takes effect at once.
    let holiday = [
        0x0E, 0x0C, 126, 11, 2, 255, 0x0F, 0x2E, 0xB4, 0, 0, 0, 0, 0x91, 0, 0x2F, 0x39, 10,
    ];
    write_ok(&mut c, sched, EXCEPTION_SCHEDULE, &holiday);
    assert_eq!(
        c.read_value(Some(2), sched, PRESENT_VALUE, None),
        [T::App(9, vec![0])]
    );
    assert_eq!(
        as_unsigned(&c.read_value(Some(2), sched, EXCEPTION_SCHEDULE, Some(0))[0]),
        1
    );
    // A calendar reference is refused.
    let calendar = [0x1C, 0x01, 0x80, 0x00, 0x01, 0x2E, 0x2F, 0x39, 10];
    assert_eq!(
        error_of(&c.write(Some(2), sched, EXCEPTION_SCHEDULE, &calendar, None)),
        (2, 37)
    );
    let writes: Vec<String> = c
        .events()
        .into_iter()
        .filter_map(|e| match e {
            ServerEvent::Write { object, text, .. } => Some(format!("{object}: {text}")),
            _ => None,
        })
        .collect();
    assert_eq!(
        writes[0],
        "OCC-SCHED: weekly-schedule[1] = 06:30 on, 18:00 off"
    );
}

/// A ReadRange-ACK: (first-item, last-item, more-items), item count, the
/// records' REAL values, first sequence number.
type RangeAck = ([bool; 3], u64, Vec<f32>, Option<u64>);

fn read_range(c: &mut Client, max_apdu: u8, range: &[u8]) -> RangeAck {
    let mut body = ctx_oid(0, (TL, 1));
    body.extend(ctx_enum(1, LOG_BUFFER));
    body.extend_from_slice(range);
    let reply = c.request_with(Some(2), max_apdu, 26, &body).unwrap();
    assert_eq!(
        &reply.payload[..3],
        [0x30, c.invoke, 26],
        "{:02X?}",
        reply.payload
    );
    let t = tags(&reply.payload[3..]);
    let T::Ctx(3, flags) = &t[2] else { panic!() };
    let flags = [0, 1, 2].map(|i| flags[1] & (0x80 >> i) != 0);
    let T::Ctx(4, count) = &t[3] else { panic!() };
    let count = count.iter().fold(0, |acc, &b| acc << 8 | b as u64);
    assert_eq!(t[4], T::Open(5));
    let close = t.iter().position(|x| *x == T::Close(5)).unwrap();
    let values: Vec<f32> = t[5..close]
        .windows(2)
        .filter_map(|w| match w {
            [T::Open(1), T::Ctx(2, v)] => Some(f32::from_be_bytes(v[..].try_into().unwrap())),
            _ => None,
        })
        .collect();
    assert_eq!(values.len() as u64, count);
    let first = t.get(close + 1).map(|x| match x {
        T::Ctx(6, s) => s.iter().fold(0, |acc, &b| acc << 8 | b as u64),
        other => panic!("{other:?}"),
    });
    (flags, count, values, first)
}

/// A range's brackets around an Unsigned (or a date and time) and an INTEGER.
fn range(tag: u8, reference: &[u8], count: i8) -> Vec<u8> {
    let mut b = vec![tag << 4 | 0x0E];
    b.extend_from_slice(reference);
    b.extend([0x31, count as u8, tag << 4 | 0x0F]);
    b
}

#[test]
fn trend_log_read_range() {
    let mut c = start();
    {
        // An hour and forty minutes of one-minute samples: 0, 1, 2 …
        let mut db = c.db.lock();
        for m in 0..100 {
            db.devices[2].objects[0].present_value = PointValue::Real(m as f32);
            db.tick(monday(6, 0) + SimDuration::minutes(m));
        }
    }
    let tl = (TL, 1);
    assert_eq!(error_of(&c.read(Some(2), tl, LOG_BUFFER, None)), (2, 27));
    assert_eq!(
        as_unsigned(&c.read_value(Some(2), tl, RECORD_COUNT, None)[0]),
        100
    );

    // Everything, for a client that takes 480 bytes: as many as fit.
    let (flags, count, values, first) = read_range(&mut c, 0x03, &[]);
    assert_eq!(flags, [true, false, true]);
    assert!((10..30).contains(&count), "{count}");
    assert_eq!(values[..3], [0.0, 1.0, 2.0]);
    assert_eq!(first, None);
    // The rest in pages, by position.
    let mut got = values.len();
    while got < 100 {
        let reference = [0x21, got as u8 + 1];
        let (flags, _, values, _) = read_range(&mut c, 0x03, &range(3, &reference, 127));
        assert_eq!(values[0], got as f32);
        got += values.len();
        assert_eq!(flags[2], got < 100);
        assert_eq!(flags[1], got == 100);
    }

    // Counting back from the newest.
    assert_eq!(
        read_range(&mut c, 0x05, &range(3, &[0x21, 100], -3)),
        ([false, true, false], 3, vec![97.0, 98.0, 99.0], None)
    );
    // By sequence number, both ways.
    assert_eq!(
        read_range(&mut c, 0x05, &range(6, &[0x21, 50], 2)),
        ([false, false, false], 2, vec![49.0, 50.0], Some(50))
    );
    assert_eq!(
        read_range(&mut c, 0x05, &range(6, &[0x21, 3], -5)),
        ([true, false, false], 3, vec![0.0, 1.0, 2.0], Some(1))
    );
    // By time: newer than 07:00, and the two before 06:30.
    let at = |h, m| [0xA4, 126, 11, 2, 1, 0xB4, h, m, 0, 0];
    assert_eq!(
        read_range(&mut c, 0x05, &range(7, &at(7, 0), 2)),
        ([false, false, false], 2, vec![61.0, 62.0], Some(62))
    );
    assert_eq!(
        read_range(&mut c, 0x05, &range(7, &at(6, 30), -2)),
        ([false, false, false], 2, vec![28.0, 29.0], Some(29))
    );
    // Not a list.
    let mut body = ctx_oid(0, tl);
    body.extend(ctx_enum(1, RECORD_COUNT));
    assert_eq!(error_of(&c.request(Some(2), 26, &body)), (2, 22));
}

/// The parts of an event notification a test looks at.
#[derive(Debug, PartialEq)]
struct Event {
    process: u64,
    object: (u16, u32),
    date: Vec<u8>,
    time: Vec<u8>,
    event_type: u8,
    notify_type: u8,
    to_state: u8,
    text: String,
}

fn event(p: &Packet) -> Event {
    let body = match p.payload[0] {
        0x10 => {
            assert_eq!(p.payload[1], 3, "not an UnconfirmedEventNotification");
            &p.payload[2..]
        }
        0x00 => {
            assert_eq!(p.payload[3], 2, "not a ConfirmedEventNotification");
            &p.payload[4..]
        }
        _ => panic!("{:02X?}", p.payload),
    };
    let t = tags(body);
    let ctx = |n: u8| -> &Vec<u8> {
        t.iter()
            .find_map(|x| match x {
                T::Ctx(m, c) if *m == n => Some(c),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no context tag {n}"))
    };
    let T::App(10, date) = &t[5] else { panic!() };
    let T::App(11, time) = &t[6] else { panic!() };
    assert_eq!(instance_of(&t[1]), (DEVICE, AHU));
    Event {
        process: ctx(0).iter().fold(0, |acc, &b| acc << 8 | b as u64),
        object: instance_of(&t[2]),
        date: date.clone(),
        time: time.clone(),
        event_type: ctx(6)[0],
        notify_type: ctx(8)[0],
        to_state: ctx(11)[0],
        text: String::from_utf8(ctx(7)[1..].to_vec()).unwrap(),
    }
}

/// A Recipient_List with one destination: every day and time, every
/// transition, and `recipient` already encoded.
fn recipient_list(recipient: &[u8], process: u8, confirmed: bool) -> Vec<u8> {
    let mut b = vec![0x82, 0x01, 0xFE, 0xB4, 0, 0, 0, 0, 0xB4, 23, 59, 59, 99];
    b.extend_from_slice(recipient);
    b.extend([0x21, process, 0x10 | confirmed as u8, 0x82, 0x05, 0xE0]);
    b
}

fn acknowledge_alarm(object: (u16, u32), state: u8, date: &[u8], time: &[u8]) -> Vec<u8> {
    let mut b = vec![0x09, 1];
    b.extend(ctx_oid(1, object));
    b.extend([0x29, state, 0x3E, 0x2E, 0xA4]);
    b.extend_from_slice(date);
    b.push(0xB4);
    b.extend_from_slice(time);
    b.extend([0x2F, 0x3F, 0x4D, 5, 0, b'T', b'e', b'c', b'h', 0x5E, 0x2E]);
    b.extend([0xA4, 126, 11, 2, 1, 0xB4, 9, 0, 0, 0, 0x2F, 0x5F]);
    b
}

/// The object ids in a GetEventInformation-ACK, and more-events.
fn event_information(c: &mut Client) -> (Vec<(u16, u32)>, bool) {
    let reply = c.request(Some(2), 29, &[]);
    assert_eq!(&reply.payload[..3], [0x30, c.invoke, 29]);
    let t = tags(&reply.payload[3..]);
    let ids = t
        .windows(2)
        .filter_map(|w| match w {
            [T::Ctx(0, id), T::Ctx(1, _)] => Some(instance_of(&T::Ctx(0, id.clone()))),
            _ => None,
        })
        .collect();
    (ids, t[t.len() - 1] == T::Ctx(1, vec![1]))
}

#[test]
fn alarms_notify_and_take_acknowledgments() {
    let mut c = start();
    let SocketAddr::V4(me) = c.sock.local_addr().unwrap() else {
        panic!()
    };
    // Point the notification class at our own address.
    let mut address = vec![0x1E, 0x21, 0x00, 0x65, 0x06];
    address.extend(me.ip().octets());
    address.extend(me.port().to_be_bytes());
    address.push(0x1F);
    let list = recipient_list(&address, 7, false);
    write_ok(&mut c, (NC, 1), RECIPIENT_LIST, &list);
    let back = c.read_value(Some(2), (NC, 1), RECIPIENT_LIST, None);
    assert_eq!(back[3], T::Open(1));
    assert_eq!(event_information(&mut c), (vec![], false));

    // The supply fan fails.
    {
        let mut db = c.db.lock();
        db.tick(monday(9, 0));
        db.devices[2].objects[2].present_value = PointValue::Binary(true);
        db.tick(monday(9, 0) + SimDuration::seconds(1));
    }
    let p = c.recv().expect("event notification");
    assert_eq!(p.snet, Some((NET, vec![2])));
    let alarm = event(&p);
    assert_eq!(
        (
            alarm.process,
            alarm.object,
            alarm.event_type,
            alarm.notify_type,
            alarm.to_state
        ),
        (7, (BI, 1), 1, 0, 2)
    );
    assert_eq!(alarm.text, "SF-FAIL alarm: On");
    assert_eq!(alarm.time, [9, 0, 1, 0]);
    // It shows as in alarm, unacknowledged.
    assert_eq!(
        c.read_value(Some(2), (BI, 1), 36, None),
        [T::App(9, vec![2])]
    );
    assert_eq!(
        c.read_value(Some(2), (BI, 1), 0, None),
        [T::App(8, vec![5, 0x60])]
    );
    assert_eq!(event_information(&mut c), (vec![(BI, 1)], false));

    // The wrong time stamp, then the right one.
    let wrong = acknowledge_alarm((BI, 1), 2, &alarm.date, &[9, 0, 0, 0]);
    assert_eq!(error_of(&c.request(Some(2), 0, &wrong)), (5, 14));
    let right = acknowledge_alarm((BI, 1), 2, &alarm.date, &alarm.time);
    assert_eq!(c.request(Some(2), 0, &right).payload, [0x20, c.invoke, 0]);
    let acked = event(&c.recv().expect("ack notification"));
    assert_eq!(
        (acked.notify_type, acked.to_state, acked.time),
        (2, 2, alarm.time)
    );
    assert!(c.events().iter().any(|e| matches!(
        e,
        ServerEvent::Acknowledged { device, object, .. } if device == "AHU-1" && object == "SF-FAIL"
    )));
    // Still in alarm, so still listed; back to normal, it drops off.
    assert_eq!(event_information(&mut c), (vec![(BI, 1)], false));
    {
        let mut db = c.db.lock();
        db.devices[2].objects[2].present_value = PointValue::Binary(false);
        db.tick(monday(9, 5));
    }
    let normal = event(&c.recv().expect("to-normal notification"));
    assert_eq!((normal.notify_type, normal.to_state), (0, 0));
    assert_eq!(event_information(&mut c), (vec![], false));
    assert_eq!(
        c.read_value(Some(2), (BI, 1), 111, None),
        [T::App(8, vec![4, 0x00])]
    );
}

#[test]
fn confirmed_notifications_to_a_device_recipient() {
    let mut c = start();
    // Device 9999, confirmed. Until it answers Who-Is, nothing is sent.
    let list = recipient_list(&[0x0C, 0x02, 0x00, 0x27, 0x0F], 3, true);
    write_ok(&mut c, (NC, 1), RECIPIENT_LIST, &list);
    {
        let mut db = c.db.lock();
        db.tick(monday(9, 0));
        db.devices[2].objects[2].present_value = PointValue::Binary(true);
        db.tick(monday(9, 0) + SimDuration::seconds(1));
    }
    assert!(c.recv_for(Duration::from_millis(500)).is_empty());

    // I-Am from device 9999 (us).
    c.send(
        None,
        None,
        None,
        &[
            0x10, 0x00, 0xC4, 0x02, 0x00, 0x27, 0x0F, 0x22, 0x05, 0xC4, 0x91, 0x03, 0x21, 0x0F,
        ],
    );
    std::thread::sleep(Duration::from_millis(100));
    {
        let mut db = c.db.lock();
        db.devices[2].objects[2].present_value = PointValue::Binary(false);
        db.tick(monday(9, 1));
    }
    let p = c.recv().expect("confirmed event notification");
    let normal = event(&p);
    assert_eq!((normal.process, normal.to_state), (3, 0));
    // Answer it the way a front end would; the device takes it quietly.
    let invoke = p.payload[2];
    c.send(Some((NET, &[2])), None, None, &[0x20, invoke, 2]);
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        !c.events()
            .iter()
            .any(|e| matches!(e, ServerEvent::Error(_)))
    );
    assert!(c.recv_for(Duration::from_millis(300)).is_empty());
}
