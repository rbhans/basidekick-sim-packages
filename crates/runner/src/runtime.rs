//! A running building: the simulation on its own thread, advanced in real
//! time, and the BACnet/IP server answering for its devices. The UI and the
//! headless runner both hold one of these.

use bask_bacnet::{BacnetServer, ServerConfig, ServerEvent};
use bask_sim::{Level, Package, Simulation};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How often the sim advances.
const TICK: Duration = Duration::from_millis(200);

#[derive(Clone, Debug)]
pub struct NetConfig {
    pub ip: Ipv4Addr,
    pub port: u16,
    pub broadcast: Option<Ipv4Addr>,
}

pub struct Runtime {
    pub sim: Arc<Mutex<Simulation>>,
    pub local: SocketAddr,
    server: Option<BacnetServer>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Runtime {
    pub fn start(package: Package, net: NetConfig) -> Result<Runtime, String> {
        let sim = Simulation::new(package, chrono::Local::now().naive_local())?;
        let (tx, rx) = mpsc::channel();
        let config = ServerConfig {
            bind_ip: net.ip,
            port: net.port,
            broadcast: net.broadcast,
        };
        let server = BacnetServer::start(config, sim.db.clone(), tx)
            .map_err(|e| format!("Couldn't open BACnet/IP on {}:{}: {e}", net.ip, net.port))?;
        let local = server.local_addr();
        let sim = Arc::new(Mutex::new(sim));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("sim".into())
            .spawn({
                let (sim, stop) = (sim.clone(), stop.clone());
                move || run(sim, rx, stop)
            })
            .map_err(|e| e.to_string())?;
        Ok(Runtime {
            sim,
            local,
            server: Some(server),
            stop,
            thread: Some(thread),
        })
    }

    pub fn requests(&self) -> u64 {
        self.server
            .as_ref()
            .map(|s| s.stats().requests)
            .unwrap_or(0)
    }

    pub fn last_client(&self) -> Option<(SocketAddr, Instant)> {
        let s = self.server.as_ref()?.stats();
        Some((s.last_client?, s.last_request?))
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        if let Some(s) = self.server.take() {
            s.stop();
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(sim: Arc<Mutex<Simulation>>, events: mpsc::Receiver<ServerEvent>, stop: Arc<AtomicBool>) {
    let mut last = Instant::now();
    // Who-Is floods the log otherwise
    let mut who_is: HashMap<SocketAddr, Instant> = HashMap::new();
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(TICK);
        let now = Instant::now();
        let elapsed = (now - last).as_secs_f64().min(2.0);
        last = now;
        let mut sim = sim.lock();
        sim.advance(elapsed);
        for event in events.try_iter() {
            let (level, text) = match event {
                ServerEvent::Listening { addr } => {
                    (Level::Info, format!("BACnet/IP listening on {addr}"))
                }
                ServerEvent::WhoIs { from } => {
                    if who_is
                        .get(&from)
                        .is_some_and(|t| now - *t < Duration::from_secs(30))
                    {
                        continue;
                    }
                    who_is.insert(from, now);
                    (Level::Bacnet, format!("Who-Is from {from}"))
                }
                ServerEvent::Write {
                    from,
                    device,
                    object,
                    text,
                } => (
                    Level::Bacnet,
                    format!("{} wrote {device} {object}: {text}", from.ip()),
                ),
                ServerEvent::Subscribed {
                    from,
                    device,
                    object,
                } => (
                    Level::Bacnet,
                    format!("{} subscribed to COV on {device} {object}", from.ip()),
                ),
                ServerEvent::Error(e) => (Level::Alarm, format!("BACnet: {e}")),
            };
            sim.note(level, text);
        }
    }
}
