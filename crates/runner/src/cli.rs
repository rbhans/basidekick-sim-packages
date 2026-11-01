//! The terminal front end: one building on BACnet/IP, a status line every
//! so often, and the log as it happens. Ctrl-C stops it.

use crate::runtime::{NetConfig, Runtime};
use bask_sim::{Level, Package, Season};
use std::net::Ipv4Addr;
use std::time::Duration;

/// `--help` text; the packages folder is filled in at run time.
pub fn usage() -> String {
    format!(
        "\
BASidekick Building Sim (terminal)

  basidekick-sim-cli [options]   run one building on BACnet/IP in this terminal
  basidekick-sim-cli --list      list the building packages, their scenarios and lessons

Options:
  --package <id or file.toml>    building to run (default small-office)
  --bind <ipv4>                  interface address to serve on (default: first LAN interface)
  --port <udp port>              BACnet/IP port (default 47808)
  --speed <x>                    sim speed, 1 = real time (default 1)
  --season <winter|mild|summer>  weather (default: today's month)
  --scenario <id>                start a scenario right away (repeatable);
                                 `mystery` breaks something at random without saying what
  --show-answers                 print what each mystery fault is (for the instructor)
  --lesson <id>                  set the building up for a lesson and print its steps

Interfaces here: {}

Your own packages: drop .toml files in
  {}
(or point BASK_SIM_PACKAGES at a folder).
",
        interface_list(),
        crate::user_packages_dir().display()
    )
}

fn interface_list() -> String {
    let list: Vec<String> = bask_bacnet::interfaces()
        .iter()
        .map(|i| format!("{} {}", i.name, i.ip))
        .collect();
    if list.is_empty() {
        "none found".into()
    } else {
        list.join(", ")
    }
}

fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

pub fn run(args: &[String]) -> Result<(), String> {
    let (packages, errors) = Package::discover(Some(&crate::user_packages_dir()));
    for e in &errors {
        eprintln!("skipped a package: {e}");
    }
    if args.iter().any(|a| a == "--list") {
        for p in &packages {
            println!("{:20} {} — {}", p.id, p.name, p.tagline);
            let mut scenarios = bask_sim::scenario::global_scenarios();
            scenarios.extend(p.scenarios.iter().cloned());
            for s in scenarios {
                println!("    {:24} {}", s.id, s.name);
            }
            for l in &p.lessons {
                println!("    lesson {:17} {} ({})", l.id, l.title, l.level);
            }
        }
        return Ok(());
    }

    let which = value(args, "--package").unwrap_or("small-office");
    let package = if which.ends_with(".toml") {
        Package::load(std::path::Path::new(which))?
    } else {
        packages
            .into_iter()
            .find(|p| p.id == which)
            .ok_or_else(|| format!("no package {which:?}; try --list"))?
    };
    let interfaces = bask_bacnet::interfaces();
    let net = match value(args, "--bind") {
        Some(ip) => {
            let ip: Ipv4Addr = ip.parse().map_err(|_| format!("bad --bind address {ip}"))?;
            let broadcast = interfaces.iter().find(|i| i.ip == ip).map(|i| i.broadcast);
            NetConfig {
                ip,
                port: 47808,
                broadcast,
            }
        }
        None => match interfaces.first() {
            Some(i) => NetConfig {
                ip: i.ip,
                port: 47808,
                broadcast: Some(i.broadcast),
            },
            None => NetConfig {
                ip: Ipv4Addr::LOCALHOST,
                port: 47808,
                broadcast: None,
            },
        },
    };
    let port = value(args, "--port")
        .map(|p| p.parse::<u16>().map_err(|_| format!("bad --port {p}")))
        .transpose()?;
    let net = NetConfig {
        port: port.unwrap_or(net.port),
        ..net
    };

    let name = package.name.clone();
    let rt = Runtime::start(package, net.clone())?;
    {
        let mut sim = rt.sim.lock();
        if let Some(id) = value(args, "--lesson") {
            sim.start_lesson(id)?;
            let l = sim.package.lessons.iter().find(|l| l.id == id).unwrap();
            println!("{} ({})\n{}\n", l.title, l.level, l.goal);
            for (i, step) in l.steps.iter().enumerate() {
                println!("{}. {}", i + 1, step.replace('`', ""));
            }
            if args.iter().any(|a| a == "--show-answers") {
                println!("\nWhat you should see: {}", l.expect.replace('`', ""));
            }
            println!();
        }
        if let Some(s) = value(args, "--speed") {
            sim.speed = s.parse().map_err(|_| format!("bad --speed {s}"))?;
        }
        match value(args, "--season") {
            Some("winter") => sim.set_season(Season::Winter),
            Some("summer") => sim.set_season(Season::Summer),
            Some("mild") => sim.set_season(Season::Mild),
            Some(other) => return Err(format!("bad --season {other}")),
            None => {}
        }
        for (i, a) in args.iter().enumerate() {
            if a == "--scenario" {
                let id = args.get(i + 1).ok_or("--scenario needs an id")?;
                if id == "mystery" {
                    let id = sim.start_mystery();
                    if args.iter().any(|a| a == "--show-answers") {
                        println!("{id}: {}", sim.mystery_answer(&id).unwrap_or_default());
                    }
                } else {
                    sim.start_scenario(id)?;
                }
            }
        }
        let devices = sim.db.lock().devices.len();
        println!(
            "{name}: {devices} BACnet devices on {} (network {}), speed {}×. Ctrl-C to stop.",
            rt.local, sim.package.network, sim.speed
        );
    }

    let mut printed = 0u64;
    let mut since_status = Duration::ZERO;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        since_status += Duration::from_millis(500);
        let sim = rt.sim.lock();
        for e in sim.log.iter().filter(|e| e.seq > printed) {
            let tag = match e.level {
                Level::Alarm => "ALARM",
                Level::Clear => "CLEAR",
                Level::Scenario => "SCENE",
                Level::Bacnet => "BACNT",
                Level::Info => "info ",
            };
            println!("{} {tag} {}", e.at.format("%a %H:%M:%S"), e.text);
        }
        printed = sim.log.back().map_or(printed, |e| e.seq);
        if since_status >= Duration::from_secs(10) {
            since_status = Duration::ZERO;
            let snap = sim.snapshot();
            let alarms: usize = snap.devices.iter().map(|d| d.alarms + d.faults).sum();
            println!(
                "{} · OA {:.1}°F · {} · {:.0} people · {:.1} kW · {} alarms · {} requests",
                snap.now.format("%a %H:%M"),
                snap.weather.oat,
                if snap.occupied {
                    "occupied"
                } else {
                    "unoccupied"
                },
                snap.people,
                snap.kw,
                alarms,
                rt.requests()
            );
        }
    }
}
