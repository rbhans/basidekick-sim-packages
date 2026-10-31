//! Serves a small demo building over BACnet/IP until killed.
//!
//! ```text
//! cargo run -p bask-bacnet --example serve_demo -- [bind-ip] [port] [broadcast]
//! ```
//!
//! Defaults to 127.0.0.1:47808. The site controller is device 2000; behind it
//! on network 2001 sit VAV-5 (device 2005, MAC 5) and AHU-1 (device 2006,
//! MAC 6). Zone temperature drifts so polls and COV subscriptions have
//! something to watch.

use bask_bacnet::{BacnetServer, ServerConfig};
use bask_points::{
    BacDevice, BacObject, ObjectType, PointDb, PointRef, PointValue, SharedDb, Units,
};
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
    std::thread::spawn(move || {
        let start = std::time::Instant::now();
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let t = start.elapsed().as_secs_f64();
            db.lock().set_real(zone_temp, 72.0 + 1.5 * (t / 20.0).sin());
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

    let mut occ = BacObject::new(
        ObjectType::BinaryValue,
        1,
        "BLDG-OCC",
        PointValue::Binary(true),
    );
    occ.commandable = true;
    occ.states = vec!["Unoccupied".into(), "Occupied".into()];
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
        ],
    );

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
            multistate(
                ObjectType::MultiStateInput,
                "AHU-MODE",
                &["Off", "Heat", "Cool", "Econ"],
            ),
        ],
    );

    let mut db = PointDb::new(2001);
    db.devices = vec![site, vav, ahu];
    Arc::new(Mutex::new(db))
}
