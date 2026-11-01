//! BACnet/IP server for the simulator's devices.
//!
//! One UDP socket serves the whole simulated building. The site controller
//! (the one device without a MAC) sits directly on BACnet/IP and is also a
//! router to a virtual network ([`PointDb::network`]) where every other device
//! lives at a one-byte MAC. A BAS discovers the site controller and, through
//! it, the routed devices, just as it would a real building with an IP
//! supervisory controller and field buses behind it.
//!
//! The server runs on its own thread. It locks the shared database only while
//! it answers one request or sends notifications, so the simulation engine is
//! never held up for long, and it reports what clients do through
//! [`ServerEvent`]s.
//!
//! Besides points, devices serve schedules, trend logs (read with
//! ReadRange) and notification classes. Event notifications the database
//! queues go out on the server's tick to each notification class's
//! recipients, and a BAS can acknowledge them and list what's active with
//! GetEventInformation.
//!
//! [`PointDb::network`]: bask_points::PointDb::network

mod alarm;
mod apdu;
mod clock;
mod cov;
mod encoding;
mod interfaces;
mod npdu;
mod properties;
mod schedule;
mod server;
mod services;
mod trend;

pub use interfaces::{NetInterface, interfaces};

use bask_points::SharedDb;
use parking_lot::Mutex;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::Instant;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerConfig {
    /// The interface to serve on; 0.0.0.0 for any. A loopback address keeps
    /// everything on 127.0.0.1, broadcasts included.
    pub bind_ip: Ipv4Addr,
    pub port: u16,
    /// The subnet's broadcast address; None uses 255.255.255.255.
    pub broadcast: Option<Ipv4Addr>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            bind_ip: Ipv4Addr::UNSPECIFIED,
            port: 47808,
            broadcast: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ServerEvent {
    Listening {
        addr: SocketAddr,
    },
    WhoIs {
        from: SocketAddr,
    },
    /// A write that took. `text` reads like `present-value = 74.0 °F @
    /// priority 8`, `relinquished priority 8` or `out-of-service = true`.
    Write {
        from: SocketAddr,
        device: String,
        object: String,
        text: String,
    },
    Subscribed {
        from: SocketAddr,
        device: String,
        object: String,
    },
    /// An AcknowledgeAlarm that took.
    Acknowledged {
        from: SocketAddr,
        device: String,
        object: String,
    },
    Error(String),
}

#[derive(Clone, Debug, Default)]
pub struct ServerStats {
    /// Requests that reached an online device: confirmed services, Who-Is,
    /// Who-Has and Who-Is-Router-To-Network.
    pub requests: u64,
    /// Property writes that took.
    pub writes: u64,
    pub last_request: Option<Instant>,
    pub last_client: Option<SocketAddr>,
}

/// A running server. Stops when dropped.
pub struct BacnetServer {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    stats: Arc<Mutex<ServerStats>>,
    local_addr: SocketAddr,
}

impl BacnetServer {
    /// Binds the socket and starts serving `db` on a new thread, announcing
    /// the router and every online device. Port 0 picks a free port.
    pub fn start(
        config: ServerConfig,
        db: SharedDb,
        events: Sender<ServerEvent>,
    ) -> io::Result<BacnetServer> {
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Mutex::new(ServerStats::default()));
        let server =
            server::Server::bind(&config, db, events.clone(), stats.clone(), stop.clone())?;
        let local_addr = server.local_addr();
        let _ = events.send(ServerEvent::Listening { addr: local_addr });
        let thread = std::thread::Builder::new()
            .name("bacnet-server".into())
            .spawn(move || server.run())?;
        Ok(BacnetServer {
            stop,
            thread: Some(thread),
            stats,
            local_addr,
        })
    }

    /// The address clients reach the site controller at.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn stats(&self) -> ServerStats {
        self.stats.lock().clone()
    }

    /// Stops the thread and closes the socket.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for BacnetServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}
