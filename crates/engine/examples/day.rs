//! Runs a package through a weekday and prints how it went:
//! `cargo run -p bask-sim --example day -- small-office summer [scenario-id] [device]`

use bask_sim::{Package, Season, Simulation};
use chrono::NaiveDate;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let id = args.first().map(String::as_str).unwrap_or("small-office");
    let season = match args.get(1).map(String::as_str) {
        Some("winter") => Season::Winter,
        Some("summer") => Season::Summer,
        _ => Season::Mild,
    };
    let scenario = args.get(2).filter(|s| *s != "-").cloned();
    let watch = args.get(3).cloned();
    let pkg = Package::builtin()
        .into_iter()
        .find(|p| p.id == id)
        .expect("no such package");
    // a Tuesday
    let start = NaiveDate::from_ymd_opt(2026, 10, 27)
        .unwrap()
        .and_hms_opt(5, 0, 0)
        .unwrap();
    let mut sim = Simulation::new(pkg, start).unwrap();
    sim.set_season(season);
    sim.speed = 60.0;
    for half_hour in 0..34 {
        if half_hour == 12
            && let Some(s) = &scenario
        {
            sim.start_scenario(s).unwrap();
        }
        for _ in 0..30 {
            sim.advance(1.0);
        }
        let snap = sim.snapshot();
        let zones: Vec<String> = snap
            .zones
            .iter()
            .map(|z| {
                format!(
                    "{}={:.1}/{:.0}",
                    z.name.split_whitespace().last().unwrap_or(""),
                    z.temp,
                    z.co2
                )
            })
            .collect();
        println!(
            "{} OA {:5.1} {} kW {:6.1} | {}",
            snap.now.format("%a %H:%M"),
            snap.weather.oat,
            if snap.occupied { "OCC  " } else { "unocc" },
            snap.kw,
            zones.join(" ")
        );
        for d in &snap.devices {
            let show = match &watch {
                Some(w) => d.name == *w,
                None => !matches!(
                    d.kind,
                    bask_sim::Kind::Vav | bask_sim::Kind::Lighting | bask_sim::Kind::Site
                ),
            };
            if show {
                println!("      {:9} {}", d.name, d.headline);
            }
        }
    }
    println!("--- log");
    for e in sim.log.iter() {
        println!("{} {:?} {}", e.at.format("%H:%M:%S"), e.level, e.text);
    }
}
