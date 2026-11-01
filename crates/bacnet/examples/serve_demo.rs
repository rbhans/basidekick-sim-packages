//! Serves a small demo building over BACnet/IP until killed.
//!
//! ```text
//! cargo run -p bask-bacnet --example serve_demo -- [bind-ip] [port] [broadcast]
//! ```
//!
//! Defaults to 127.0.0.1:47808. The site controller is device 2000; behind it
//! on network 2001 sit VAV-5 (device 2005, MAC 5) and AHU-1 (device 2006,
//! MAC 6). Zone temperature drifts so polls and COV subscriptions have
//! something to watch, and its swings trip ZN-T's high and low limits; the
//! supply fan "fails" for 30 s of every 2 minutes. Time runs on the host's
//! clock here, ticked once a second.
//!
//! - SITE-CTRL: OCC-SCHED (schedule 1, weekdays 07:00–18:00, Saturday
//!   08:00–12:00), OA-T-TL (trend log 1, every 10 s).
//! - VAV-5: ZN-T reports out-of-range (71–73 °F), ZN-T-TL (trend log 1,
//!   every 5 s), and its notification class 1.
//! - AHU-1: SF-FAIL reports change-of-state, with its own notification
//!   class 1.
//!
//! The notification classes broadcast on the local network. On loopback
//! that's the server's own port, so point Recipient_List at your client
//! (by address, or by device for one that answers Who-Is) to see them.

use bask_bacnet::{BacnetServer, ServerConfig};
use bask_points::{
    BacDevice, BacObject, ObjectType, PointDb, PointRef, PointValue, SharedDb, Units,
};
use chrono::NaiveTime;
use parking_lot::Mutex;
use std::net::Ipv4Addr;
use std::sync::{Arc, mpsc};
use std::time::Duration;

fn main() {
    let mut args = std::env::args().skip(1);
    let bind_ip: Ipv4Addr = args
        .next()
        .map_or(Ipv4Addr::LOCALHOST, |s| s.parse().expect("bind ip"));
    let port: u16 = args.next().map_or(47808, |s| s.parse().expect("port"));
    let broadcast = args.next().map(|s| s.parse().expect("broadcast address"));

    for i in bask_bacnet::interfaces() {
        println!(
            "interface {} {} mask {} broadcast {}",
            i.name, i.ip, i.netmask, i.broadcast
        );
    }
    let db = demo_db();
    let (tx, rx) = mpsc::channel();
    let server = BacnetServer::start(
        ServerConfig {
            bind_ip,
            port,
            broadcast,
        },
        db.clone(),
        tx,
    )
    .expect("start server");
    println!("serving on {}", server.local_addr());

    // A stand-in for the simulation engine.
    let zone_temp = PointRef {
        device: 1,
        object: 0,
    };
    let fan_fail = PointRef {
        device: 2,
        object: 3,
    };
    std::thread::spawn(move || {
        let start = std::time::Instant::now();
        let mut shown = Vec::new();
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let t = start.elapsed().as_secs_f64();
            let mut db = db.lock();
            db.set_real(zone_temp, 72.0 + 1.5 * (t / 20.0).sin());
            db.set_bool(fan_fail, t % 120.0 >= 90.0);
            db.tick(chrono::Local::now().naive_local());
            let states: Vec<_> = [zone_temp, fan_fail]
                .map(|r| (db.obj(r).name.clone(), db.obj(r).event_state()))
                .into();
            if states != shown {
                println!("event states: {states:?}");
                shown = states;
            }
        }
    });

    for event in rx {
        println!("{event:?}");
    }
}

fn demo_db() -> SharedDb {
    let device = |instance, name: &str, mac, objects| BacDevice {
        instance,
        name: name.into(),
        description: format!("Demo {name}"),
        location: "Demo building".into(),
        model_name: "BASidekick Sim".into(),
        mac,
        online: true,
        objects,
        database_revision: 1,
    };
    let analog = |t, instance, name: &str, value: f32, units| {
        let mut o = BacObject::new(t, instance, name, PointValue::Real(value));
        o.units = units;
        o
    };
    let multistate = |t, name: &str, states: &[&str]| {
        let mut o = BacObject::new(t, 1, name, PointValue::Multi(1));
        o.states = states.iter().map(|s| s.to_string()).collect();
        o.commandable |= t == ObjectType::MultiStateValue;
        o
    };
    let hm = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();

    let mut occ = BacObject::new(
        ObjectType::BinaryValue,
        1,
        "BLDG-OCC",
        PointValue::Binary(true),
    );
    occ.commandable = true;
    occ.states = vec!["Unoccupied".into(), "Occupied".into()];
    let weekday = vec![(hm(7, 0), true), (hm(18, 0), false)];
    let mut sched = BacObject::schedule(
        1,
        "OCC-SCHED",
        [
            weekday.clone(),
            weekday.clone(),
            weekday.clone(),
            weekday.clone(),
            weekday,
            vec![(hm(8, 0), true), (hm(12, 0), false)],
            vec![],
        ],
    );
    sched.description = "Building occupancy".into();
    let site = device(
        2000,
        "SITE-CTRL",
        None,
        vec![
            analog(
                ObjectType::AnalogInput,
                1,
                "OA-T",
                55.0,
                Units::DegreesFahrenheit,
            ),
            occ,
            sched,
            BacObject::trend_log(1, "OA-T-TL", (ObjectType::AnalogInput, 1), 10, 1000),
        ],
    );

    let mut zn_t = analog(
        ObjectType::AnalogInput,
        1,
        "ZN-T",
        72.0,
        Units::DegreesFahrenheit,
    );
    zn_t.cov_increment = 0.2;
    zn_t.enable_out_of_range(71.0, 73.0, 0.2, 1, 5);
    let mut zn_sp = analog(
        ObjectType::AnalogValue,
        1,
        "ZN-SP",
        70.0,
        Units::DegreesFahrenheit,
    );
    zn_sp.commandable = true;
    zn_sp.min_value = Some(55.0);
    zn_sp.max_value = Some(85.0);
    let vav = device(
        2005,
        "VAV-5",
        Some(5),
        vec![
            zn_t,
            zn_sp,
            analog(
                ObjectType::AnalogOutput,
                1,
                "DMPR-POS",
                35.0,
                Units::Percent,
            ),
            multistate(
                ObjectType::MultiStateValue,
                "OCC-MODE",
                &["Auto", "Occupied", "Unoccupied"],
            ),
            BacObject::trend_log(1, "ZN-T-TL", (ObjectType::AnalogInput, 1), 5, 2000),
            BacObject::notification_class(1, "ALARMS"),
        ],
    );

    let mut sf_fail = BacObject::new(
        ObjectType::BinaryInput,
        2,
        "SF-FAIL",
        PointValue::Binary(false),
    );
    sf_fail.states = vec!["Normal".into(), "Alarm".into()];
    sf_fail.enable_change_of_state(true, 1, 0);
    let ahu = device(
        2006,
        "AHU-1",
        Some(6),
        vec![
            analog(
                ObjectType::AnalogInput,
                1,
                "SA-T",
                55.0,
                Units::DegreesFahrenheit,
            ),
            BacObject::new(
                ObjectType::BinaryOutput,
                1,
                "SF-C",
                PointValue::Binary(true),
            ),
            BacObject::new(ObjectType::BinaryInput, 1, "SF-S", PointValue::Binary(true)),
            sf_fail,
            multistate(
                ObjectType::MultiStateInput,
                "AHU-MODE",
                &["Off", "Heat", "Cool", "Econ"],
            ),
            BacObject::notification_class(1, "ALARMS"),
        ],
    );

    let mut db = PointDb::new(2001);
    db.devices = vec![site, vav, ahu];
    Arc::new(Mutex::new(db))
}
