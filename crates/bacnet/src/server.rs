//! The server thread: one UDP socket, routing between BACnet/IP and the
//! virtual network, and the timer that sends COV and event notifications.
//!
//! Every packet is handled start to finish under one lock of the database;
//! replies and events are collected while it's held and sent after it's
//! released. Messages with no DNET go to the site controller; DNET equal to
//! the virtual network reaches the routed device at DADR. Routed devices
//! answer with SNET/SADR set, and anything that came through another router
//! is answered back through it with DNET/DADR.

use crate::alarm::{self, Notifier, Outgoing, Route};
use crate::apdu::{
    self, ABORT_SEGMENTATION_NOT_SUPPORTED, Apdu, ConfirmedHeader, MAX_APDU, confirmed, unconfirmed,
};
use crate::cov::{CovOutcome, CovTable, Notification};
use crate::encoding::ctx_unsigned;
use crate::npdu::{self, Frame, GLOBAL_NETWORK, NetAddr, Npdu, Peer};
use crate::services::{self, Reply};
use crate::trend;
use crate::{ServerConfig, ServerEvent, ServerStats, interfaces};
use bask_points::{PointDb, SharedDb};
use parking_lot::Mutex;
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

/// How long a receive waits, and so how quickly the thread notices stop.
const POLL: Duration = Duration::from_millis(100);
const COV_INTERVAL: Duration = Duration::from_millis(300);
/// How long our own broadcasts are remembered for spotting their echoes.
const ECHO_WINDOW: Duration = Duration::from_secs(5);
/// Repeats of the same error are reported at most this often.
const ERROR_QUIET: Duration = Duration::from_secs(10);

/// A packet to send.
struct Out {
    to: SocketAddrV4,
    broadcast: bool,
    npdu: Vec<u8>,
}

/// What handling one packet produced, held until the database is unlocked.
#[derive(Default)]
struct Pending {
    out: Vec<Out>,
    events: Vec<ServerEvent>,
}

/// Which devices an NPDU's destination reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dest {
    /// No DNET: the BACnet/IP network itself.
    Local,
    /// DNET 0xFFFF.
    Global,
    /// The virtual network, every station.
    RoutedBroadcast,
    Routed(u8),
    /// A network we don't route to.
    Elsewhere(u16),
    /// The virtual network, but an address no station there can have.
    Nowhere,
}

impl Dest {
    fn of(dest: Option<&NetAddr>, network: u16) -> Dest {
        match dest {
            None => Dest::Local,
            Some(d) if d.net == GLOBAL_NETWORK => Dest::Global,
            Some(d) if d.net == network => match d.mac[..] {
                [] => Dest::RoutedBroadcast,
                [mac] => Dest::Routed(mac),
                _ => Dest::Nowhere,
            },
            Some(d) => Dest::Elsewhere(d.net),
        }
    }
}

pub struct Server {
    socket: UdpSocket,
    db: SharedDb,
    events: Sender<ServerEvent>,
    stats: Arc<Mutex<ServerStats>>,
    stop: Arc<AtomicBool>,
    local_addr: SocketAddrV4,
    /// Addresses our own packets come back from, with our port.
    own_ips: Vec<Ipv4Addr>,
    broadcast: SocketAddrV4,
    /// When and what we broadcast lately, to drop the echoes.
    recent_broadcasts: VecDeque<(Instant, u64)>,
    cov: CovTable,
    /// Where event notifications go.
    notifier: Notifier,
    /// Whether each device (by instance) was reachable at the last tick, to
    /// announce the ones that come back.
    reachable: HashMap<u32, bool>,
    last_error: Option<(String, Instant)>,
    /// Reported once the thread starts, after Listening.
    startup_warning: Option<String>,
}

impl Server {
    pub fn bind(
        config: &ServerConfig,
        db: SharedDb,
        events: Sender<ServerEvent>,
        stats: Arc<Mutex<ServerStats>>,
        stop: Arc<AtomicBool>,
    ) -> io::Result<Server> {
        let loopback = config.bind_ip.is_loopback();
        // Sharing the port (below) means binding succeeds even when another
        // BACnet program holds it, and then that program may get the unicast
        // requests meant for us. Find out first so the user can be told.
        let startup_warning = (!loopback
            && config.port != 0
            && UdpSocket::bind((Ipv4Addr::UNSPECIFIED, config.port)).is_err())
        .then(|| {
            format!(
                "UDP port {} is also open in another program; requests meant for the simulator may go to it instead",
                config.port
            )
        });
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        let bind_to = if loopback {
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, config.port)
        } else {
            // A socket bound to a unicast address doesn't see subnet
            // broadcasts on macOS, so take the port on every interface.
            socket.set_reuse_address(true)?;
            #[cfg(all(
                unix,
                not(any(target_os = "solaris", target_os = "illumos", target_os = "cygwin"))
            ))]
            socket.set_reuse_port(true)?;
            socket.set_broadcast(true)?;
            SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, config.port)
        };
        socket.bind(&bind_to.into())?;
        let socket: UdpSocket = socket.into();
        socket.set_read_timeout(Some(POLL))?;
        let port = socket.local_addr()?.port();

        let (local_ip, broadcast_ip, own_ips) = if loopback {
            (
                Ipv4Addr::LOCALHOST,
                Ipv4Addr::LOCALHOST,
                vec![Ipv4Addr::LOCALHOST],
            )
        } else {
            let mut own: Vec<Ipv4Addr> = interfaces().into_iter().map(|i| i.ip).collect();
            own.push(config.bind_ip);
            (
                config.bind_ip,
                config.broadcast.unwrap_or(Ipv4Addr::BROADCAST),
                own,
            )
        };
        Ok(Server {
            socket,
            db,
            events,
            stats,
            stop,
            local_addr: SocketAddrV4::new(local_ip, port),
            own_ips,
            broadcast: SocketAddrV4::new(broadcast_ip, port),
            recent_broadcasts: VecDeque::new(),
            cov: CovTable::default(),
            notifier: Notifier::default(),
            reachable: HashMap::new(),
            last_error: None,
            startup_warning,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        SocketAddr::V4(self.local_addr)
    }

    pub fn run(mut self) {
        if let Some(warning) = self.startup_warning.take() {
            self.error(warning);
        }
        let mut buf = [0u8; 2048];
        let mut next_tick = Instant::now();
        while !self.stop.load(Ordering::Relaxed) {
            let now = Instant::now();
            if now >= next_tick {
                // The first tick announces every reachable device.
                if catch_unwind(AssertUnwindSafe(|| self.tick(now))).is_err() {
                    self.error("internal error sending notifications".into());
                }
                next_tick = now + COV_INTERVAL;
            }
            match self.socket.recv_from(&mut buf) {
                Ok((n, src)) => {
                    // A bug handling one packet shouldn't take the network down.
                    if catch_unwind(AssertUnwindSafe(|| self.packet(&buf[..n], src))).is_err() {
                        self.error(format!("internal error handling a packet from {src}"));
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                            | io::ErrorKind::ConnectionReset
                    ) => {}
                Err(e) => {
                    self.error(format!("receive failed: {e}"));
                    std::thread::sleep(POLL);
                }
            }
        }
    }

    fn packet(&mut self, pkt: &[u8], src: SocketAddr) {
        let SocketAddr::V4(src) = src else { return };
        let Some(frame) = npdu::parse_bvlc(pkt) else {
            return;
        };
        let db = self.db.clone();
        let (npdu_bytes, broadcast, from) = match frame {
            Frame::Nak(code) => {
                if router_up(&db.lock()) {
                    self.send_to(src, &npdu::bvlc_result(code));
                }
                return;
            }
            Frame::Npdu {
                npdu,
                broadcast,
                origin,
            } => (npdu, broadcast, origin.unwrap_or(src)),
        };
        if self.is_echo(from, npdu_bytes) {
            return;
        }
        let Some((header, payload)) = npdu::parse_npdu(npdu_bytes) else {
            return;
        };
        let peer = Peer {
            addr: from,
            remote: header.source.clone(),
        };

        let mut p = Pending::default();
        {
            let mut db = db.lock();
            // The site controller is the router: while it's off the network,
            // nothing behind it answers either.
            if !router_up(&db) || header.source.as_ref().is_some_and(|s| s.net == db.network) {
                return;
            }
            match header.message {
                Some(message) => {
                    self.network_message(&db, &header, message, payload, &peer, &mut p)
                }
                None => self.application(&mut db, &header, payload, &peer, broadcast, &mut p),
            }
        }
        for o in p.out {
            self.send(o);
        }
        for e in p.events {
            let _ = self.events.send(e);
        }
    }

    fn network_message(
        &self,
        db: &PointDb,
        header: &Npdu,
        message: u8,
        data: &[u8],
        peer: &Peer,
        p: &mut Pending,
    ) {
        if header
            .dest
            .as_ref()
            .is_some_and(|d| d.net != GLOBAL_NETWORK)
            || message != npdu::WHO_IS_ROUTER_TO_NETWORK
        {
            return;
        }
        let asked = data.get(..2).map(|b| u16::from_be_bytes([b[0], b[1]]));
        if asked.is_some_and(|n| n != db.network) {
            return;
        }
        self.count_request(peer);
        // Broadcast as the standard says, and straight back to the asker,
        // who may not hear broadcasts (a tool on loopback, say).
        let data = db.network.to_be_bytes();
        let msg = Some(npdu::I_AM_ROUTER_TO_NETWORK);
        p.out.push(Out {
            to: self.broadcast,
            broadcast: true,
            npdu: npdu::build_npdu(
                &Npdu {
                    message: msg,
                    ..Default::default()
                },
                &data,
            ),
        });
        p.out.push(Out {
            to: peer.addr,
            broadcast: false,
            npdu: npdu::build_npdu(
                &Npdu {
                    dest: peer.remote.clone(),
                    message: msg,
                    ..Default::default()
                },
                &data,
            ),
        });
    }

    fn application(
        &mut self,
        db: &mut PointDb,
        header: &Npdu,
        apdu: &[u8],
        peer: &Peer,
        broadcast: bool,
        p: &mut Pending,
    ) {
        let Some(apdu) = apdu::parse_apdu(apdu) else {
            return;
        };
        let dest = Dest::of(header.dest.as_ref(), db.network);
        match apdu {
            Apdu::Unconfirmed(service, body) => self.unconfirmed(db, dest, service, body, peer, p),
            Apdu::Confirmed(h, body) => {
                let dev = match dest {
                    Dest::Local => db.local_device(),
                    Dest::Routed(mac) => db.device_at_mac(mac).filter(|&i| db.devices[i].online),
                    Dest::Elsewhere(net) => {
                        if !broadcast {
                            p.out.push(reject_message_to_network(peer, net));
                        }
                        None
                    }
                    Dest::Global | Dest::RoutedBroadcast | Dest::Nowhere => None,
                };
                if let Some(dev) = dev {
                    self.count_request(peer);
                    self.confirmed(db, dev, h, body, peer, p);
                }
            }
            Apdu::Other => {}
        }
    }

    fn unconfirmed(
        &mut self,
        db: &PointDb,
        dest: Dest,
        service: u8,
        body: &[u8],
        peer: &Peer,
        p: &mut Pending,
    ) {
        // An I-Am may be from a device an alarm recipient list names.
        if service == unconfirmed::I_AM {
            self.notifier.i_am(db, body, peer);
            return;
        }
        // A real router wouldn't pass a local broadcast on to its other
        // network, but some tools only send local Who-Is and then talk to
        // routed devices through the router, so local requests reach every
        // device too.
        let online = |i: &usize| db.devices[*i].online;
        let devices: Vec<usize> = match dest {
            Dest::Local | Dest::Global => (0..db.devices.len()).filter(online).collect(),
            Dest::RoutedBroadcast => (0..db.devices.len())
                .filter(|&i| db.devices[i].mac.is_some() && online(&i))
                .collect(),
            Dest::Routed(mac) => db.device_at_mac(mac).filter(online).into_iter().collect(),
            Dest::Elsewhere(_) | Dest::Nowhere => return,
        };
        let answers: Vec<(usize, Vec<u8>)> = match service {
            unconfirmed::WHO_IS => {
                p.events.push(ServerEvent::WhoIs {
                    from: SocketAddr::V4(peer.addr),
                });
                let Ok(range) = services::who_is_range(body) else {
                    return;
                };
                devices
                    .into_iter()
                    .filter(|&i| services::in_range(range, db.devices[i].instance))
                    .map(|i| (i, services::i_am(db, i)))
                    .collect()
            }
            unconfirmed::WHO_HAS => {
                let Ok(who_has) = services::parse_who_has(body) else {
                    return;
                };
                devices
                    .into_iter()
                    .filter_map(|i| Some((i, services::i_have(db, i, &who_has)?)))
                    .collect()
            }
            // Time synchronization (the sim owns the clock) and the rest need
            // no answer.
            _ => return,
        };
        self.count_request(peer);
        // Straight back to the asker, and broadcast as the standard has it.
        for (dev, apdu) in answers {
            p.out.push(reply(db, dev, peer, &apdu, false));
            p.out.push(broadcast_from(db, dev, &apdu, self.broadcast));
        }
    }

    fn confirmed(
        &mut self,
        db: &mut PointDb,
        dev: usize,
        h: ConfirmedHeader,
        body: &[u8],
        peer: &Peer,
        p: &mut Pending,
    ) {
        // We don't segment, in either direction.
        let too_big = apdu::abort(h.invoke_id, ABORT_SEGMENTATION_NOT_SUPPORTED);
        if h.segmented {
            p.out.push(reply(db, dev, peer, &too_big, false));
            return;
        }
        let from = SocketAddr::V4(peer.addr);
        let max_apdu = h.max_apdu.min(MAX_APDU);
        let mut writes = Vec::new();
        let mut initial_notification = None;
        let result = match h.service {
            confirmed::READ_PROPERTY => services::read_property(db, dev, body),
            confirmed::READ_PROPERTY_MULTIPLE => services::read_property_multiple(db, dev, body),
            confirmed::WRITE_PROPERTY => services::write_property(db, dev, body, &mut writes),
            confirmed::WRITE_PROPERTY_MULTIPLE => {
                services::write_property_multiple(db, dev, body, &mut writes)
            }
            confirmed::SUBSCRIBE_COV => self
                .cov
                .subscribe(db, dev, peer, body, Instant::now())
                .map(|r| match r {
                    Ok(CovOutcome::Subscribed { object, initial }) => {
                        let device = db.devices[dev].name.clone();
                        p.events.push(ServerEvent::Subscribed {
                            from,
                            device,
                            object,
                        });
                        initial_notification = Some(initial);
                        Reply::SimpleAck
                    }
                    Ok(CovOutcome::Cancelled) => Reply::SimpleAck,
                    Err(e) => Reply::Error(e),
                }),
            confirmed::READ_RANGE => trend::read_range(db, dev, body, max_apdu),
            confirmed::ACKNOWLEDGE_ALARM => alarm::acknowledge(db, dev, body).map(|r| match r {
                Ok(object) => {
                    let device = db.devices[dev].name.clone();
                    p.events.push(ServerEvent::Acknowledged {
                        from,
                        device,
                        object,
                    });
                    Reply::SimpleAck
                }
                Err(e) => Reply::Error(e),
            }),
            confirmed::GET_EVENT_INFORMATION => {
                alarm::get_event_information(db, dev, body, max_apdu)
            }
            confirmed::DEVICE_COMMUNICATION_CONTROL | confirmed::REINITIALIZE_DEVICE => {
                Ok(Reply::SimpleAck)
            }
            _ => Ok(Reply::Reject(apdu::REJECT_UNRECOGNIZED_SERVICE)),
        };
        let mut reply_apdu = result
            .unwrap_or_else(|e| Reply::Reject(e.reject_reason()))
            .into_apdu(h.invoke_id, h.service);
        if reply_apdu.len() > max_apdu {
            reply_apdu = too_big;
        }
        p.out.push(reply(db, dev, peer, &reply_apdu, false));
        if let Some(n) = initial_notification {
            p.out.push(notification(db, n));
        }
        if !writes.is_empty() {
            self.stats.lock().writes += writes.len() as u64;
        }
        p.events
            .extend(writes.into_iter().map(|w| ServerEvent::Write {
                from,
                device: w.device,
                object: w.object,
                text: w.text,
            }));
    }

    /// COV checks, event notifications, Who-Is for alarm recipients not yet
    /// found, and I-Am from devices that just came (back) onto the network:
    /// all of them on the first tick.
    fn tick(&mut self, now: Instant) {
        let db = self.db.clone();
        let mut out = Vec::new();
        {
            let mut db = db.lock();
            let notices = db.take_notices();
            let router_up = router_up(&db);
            let mut reachable = HashMap::with_capacity(db.devices.len());
            // The site controller first, so the router announcement leads.
            let order = db
                .local_device()
                .into_iter()
                .chain((0..db.devices.len()).filter(|&i| db.devices[i].mac.is_some()));
            for i in order {
                let d = &db.devices[i];
                let up = router_up && d.online;
                if up && !self.reachable.get(&d.instance).copied().unwrap_or(false) {
                    if d.mac.is_none() {
                        out.push(Out {
                            to: self.broadcast,
                            broadcast: true,
                            npdu: npdu::build_npdu(
                                &Npdu {
                                    message: Some(npdu::I_AM_ROUTER_TO_NETWORK),
                                    ..Default::default()
                                },
                                &db.network.to_be_bytes(),
                            ),
                        });
                    }
                    out.push(broadcast_from(
                        &db,
                        i,
                        &services::i_am(&db, i),
                        self.broadcast,
                    ));
                }
                reachable.insert(d.instance, up);
            }
            self.reachable = reachable;
            out.extend(
                self.cov
                    .tick(&db, now)
                    .into_iter()
                    .map(|n| notification(&db, n)),
            );
            // Devices that are off the network (or behind a router that is)
            // miss their notifications, as real ones would.
            for n in &notices {
                let up = db.device_index(n.device).is_some_and(|i| {
                    self.reachable
                        .get(&db.devices[i].instance)
                        .copied()
                        .unwrap_or(false)
                });
                if up {
                    for o in self.notifier.route(&db, n) {
                        out.push(self.outgoing(&db, o));
                    }
                }
            }
            if router_up {
                for device in self.notifier.unbound(&db, now) {
                    out.push(self.who_is(device));
                }
            }
        }
        for o in out {
            self.send(o);
        }
    }

    fn outgoing(&self, db: &PointDb, o: Outgoing) -> Out {
        match o.route {
            Route::Peer(peer) => reply(db, o.device, &peer, &o.apdu, o.confirmed),
            Route::LocalBroadcast => broadcast_from(db, o.device, &o.apdu, self.broadcast),
            Route::Remote(addr) => Out {
                to: self.broadcast,
                broadcast: true,
                npdu: npdu::build_npdu(
                    &Npdu {
                        dest: Some(addr),
                        source: routed_address(db, o.device),
                        expecting_reply: o.confirmed,
                        message: None,
                    },
                    &o.apdu,
                ),
            },
        }
    }

    /// A global Who-Is for one device, from the site controller.
    fn who_is(&self, device: u32) -> Out {
        let mut range = Vec::new();
        ctx_unsigned(&mut range, 0, device as u64);
        ctx_unsigned(&mut range, 1, device as u64);
        Out {
            to: self.broadcast,
            broadcast: true,
            npdu: npdu::build_npdu(
                &Npdu {
                    dest: Some(NetAddr {
                        net: GLOBAL_NETWORK,
                        mac: Vec::new(),
                    }),
                    ..Default::default()
                },
                &apdu::unconfirmed_request(unconfirmed::WHO_IS, &range),
            ),
        }
    }

    fn count_request(&self, peer: &Peer) {
        let mut stats = self.stats.lock();
        stats.requests += 1;
        stats.last_request = Some(Instant::now());
        stats.last_client = Some(SocketAddr::V4(peer.addr));
    }

    fn send(&mut self, o: Out) {
        let function = if o.broadcast {
            self.remember_broadcast(&o.npdu);
            npdu::ORIGINAL_BROADCAST
        } else {
            npdu::ORIGINAL_UNICAST
        };
        self.send_to(o.to, &npdu::bvlc(function, &o.npdu));
    }

    fn send_to(&mut self, to: SocketAddrV4, packet: &[u8]) {
        if let Err(e) = self.socket.send_to(packet, to) {
            self.error(format!("send to {to} failed: {e}"));
        }
    }

    fn remember_broadcast(&mut self, npdu: &[u8]) {
        let now = Instant::now();
        while self
            .recent_broadcasts
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > ECHO_WINDOW)
        {
            self.recent_broadcasts.pop_front();
        }
        self.recent_broadcasts.push_back((now, fingerprint(npdu)));
    }

    /// One of our own broadcasts coming back to us.
    fn is_echo(&self, from: SocketAddrV4, npdu: &[u8]) -> bool {
        if from.port() != self.local_addr.port() || !self.own_ips.contains(from.ip()) {
            return false;
        }
        let f = fingerprint(npdu);
        let now = Instant::now();
        self.recent_broadcasts
            .iter()
            .any(|(t, x)| *x == f && now.duration_since(*t) <= ECHO_WINDOW)
    }

    fn error(&mut self, message: String) {
        let now = Instant::now();
        if let Some((last, at)) = &self.last_error
            && *last == message
            && now.duration_since(*at) < ERROR_QUIET
        {
            return;
        }
        let _ = self.events.send(ServerEvent::Error(message.clone()));
        self.last_error = Some((message, now));
    }
}

fn router_up(db: &PointDb) -> bool {
    db.local_device().is_some_and(|i| db.devices[i].online)
}

fn fingerprint(npdu: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    npdu.hash(&mut h);
    h.finish()
}

/// A routed device's address on the virtual network.
fn routed_address(db: &PointDb, dev: usize) -> Option<NetAddr> {
    db.devices[dev].mac.map(|mac| NetAddr {
        net: db.network,
        mac: vec![mac],
    })
}

/// An APDU from device `dev` to `peer`, through the peer's router if it has one.
fn reply(db: &PointDb, dev: usize, peer: &Peer, apdu: &[u8], expecting_reply: bool) -> Out {
    let header = Npdu {
        dest: peer.remote.clone(),
        source: routed_address(db, dev),
        expecting_reply,
        message: None,
    };
    Out {
        to: peer.addr,
        broadcast: false,
        npdu: npdu::build_npdu(&header, apdu),
    }
}

fn broadcast_from(db: &PointDb, dev: usize, apdu: &[u8], to: SocketAddrV4) -> Out {
    let header = Npdu {
        source: routed_address(db, dev),
        ..Default::default()
    };
    Out {
        to,
        broadcast: true,
        npdu: npdu::build_npdu(&header, apdu),
    }
}

fn notification(db: &PointDb, n: Notification) -> Out {
    reply(db, n.device, &n.peer, &n.apdu, n.confirmed)
}

fn reject_message_to_network(peer: &Peer, net: u16) -> Out {
    let [hi, lo] = net.to_be_bytes();
    // Reason 1: not directly connected and no route known.
    let header = Npdu {
        dest: peer.remote.clone(),
        message: Some(npdu::REJECT_MESSAGE_TO_NETWORK),
        ..Default::default()
    };
    Out {
        to: peer.addr,
        broadcast: false,
        npdu: npdu::build_npdu(&header, &[1, hi, lo]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_broadcasts_are_echoes() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let config = ServerConfig {
            bind_ip: Ipv4Addr::LOCALHOST,
            port: 0,
            broadcast: None,
        };
        let db = Arc::new(Mutex::new(PointDb::new(2001)));
        let mut s = Server::bind(&config, db, tx, Default::default(), Default::default()).unwrap();
        let me = s.local_addr;
        assert_eq!(s.broadcast, me);
        s.remember_broadcast(&[0x01, 0x00, 0x10, 0x00]);
        assert!(s.is_echo(me, &[0x01, 0x00, 0x10, 0x00]));
        assert!(!s.is_echo(me, &[0x01, 0x00, 0x10, 0x08]));
        assert!(!s.is_echo(
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, me.port().wrapping_add(1)),
            &[0x01, 0x00, 0x10, 0x00]
        ));
    }

    #[test]
    fn destinations() {
        let n = |net, mac: &[u8]| {
            Some(NetAddr {
                net,
                mac: mac.to_vec(),
            })
        };
        assert_eq!(Dest::of(None, 2001), Dest::Local);
        assert_eq!(Dest::of(n(0xFFFF, &[]).as_ref(), 2001), Dest::Global);
        assert_eq!(Dest::of(n(2001, &[]).as_ref(), 2001), Dest::RoutedBroadcast);
        assert_eq!(Dest::of(n(2001, &[5]).as_ref(), 2001), Dest::Routed(5));
        assert_eq!(Dest::of(n(2001, &[5, 6]).as_ref(), 2001), Dest::Nowhere);
        assert_eq!(Dest::of(n(7, &[5]).as_ref(), 2001), Dest::Elsewhere(7));
    }
}
