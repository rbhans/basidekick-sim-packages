//! The server as a BAS sees it: raw BACnet/IP packets over loopback.
//!
//! Requests are built by hand and replies picked apart with a minimal tag
//! walker, independent of the crate's own encoder.

use bask_bacnet::{BacnetServer, ServerConfig, ServerEvent};
use bask_points::{BacDevice, BacObject, ObjectType, PointDb, PointValue, SharedDb, Units};
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
const MSV: u16 = 19;
const DEVICE: u16 = 8;

const PRESENT_VALUE: u32 = 85;
const PRIORITY_ARRAY: u32 = 87;
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

    let sa_t = BacObject::new(ObjectType::AnalogInput, 1, "SA-T", PointValue::Real(55.0));
    let sf_c = BacObject::new(
        ObjectType::BinaryOutput,
        1,
        "SF-C",
        PointValue::Binary(true),
    );

    let mut db = PointDb::new(NET);
    db.devices = vec![
        device(SITE, "SITE", None, vec![oat]),
        device(VAV, "VAV-1", Some(1), vec![zn_t, zn_sp, dmpr, occ]),
        device(AHU, "AHU-1", Some(2), vec![sa_t, sf_c]),
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
        let mut body = ctx_oid(0, object);
        body.extend(ctx_enum(1, property));
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

    // Unrecognized service (ReadRange) → Reject 9; malformed → Reject 5.
    assert_eq!(
        c.request(None, 26, &ctx_oid(0, (AI, 1))).payload,
        [0x60, c.invoke, 9]
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
