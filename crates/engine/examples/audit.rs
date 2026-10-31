//! Runs every building with nothing fired, across seasons, days and times,
//! and reports anything a BAS would flag: alarms, faults, zones out of their
//! band while occupied, and heating and cooling at once.
//! `cargo run --release -p bask-sim --example audit`

use bask_sim::{Package, Season, Simulation};
use chrono::NaiveDate;

fn main() {
    let mut problems = 0;
    for pkg in Package::builtin() {
        for season in Season::ALL {
            // Tuesday and Saturday
            for (day, date) in [("Tue", 27), ("Sat", 31)] {
                for hour in [3, 7, 10, 14, 19] {
                    let start = NaiveDate::from_ymd_opt(2026, 10, date)
                        .unwrap()
                        .and_hms_opt(hour, 0, 0)
                        .unwrap();
                    let mut sim = Simulation::new(pkg.clone(), start).unwrap();
                    sim.set_season(season);
                    sim.speed = 60.0;
                    let mut seen = Vec::new();
                    for minute in 0..180 {
                        sim.advance(1.0);
                        if minute % 10 != 9 {
                            continue;
                        }
                        let snap = sim.snapshot();
                        for d in &snap.devices {
                            for p in d.points.iter().filter(|p| p.alarm || p.fault) {
                                seen.push(format!("{} {} {}", d.name, p.name, p.value));
                            }
                        }
                        // comfort counts once people are due in
                        let h = bask_sim::world::hour_of(snap.now);
                        if snap
                            .hours
                            .is_some_and(|(open, close)| h >= open && h < close)
                        {
                            for z in &snap.zones {
                                let off = z.off_band();
                                if off.abs() > 2.0 && z.setpoints.is_some_and(|(c, _)| c < 95.0) {
                                    seen.push(format!(
                                        "{} {:.1}°F, {:+.1} outside {}",
                                        z.name,
                                        z.temp,
                                        off,
                                        z.band_text()
                                    ));
                                }
                            }
                        }
                        for d in &snap.devices {
                            let v = |n: &str| {
                                d.points
                                    .iter()
                                    .find(|p| p.name == n)
                                    .map(|p| p.value.clone())
                                    .unwrap_or_default()
                            };
                            let on = |n: &str| {
                                v(n) == "On"
                                    || v(n)
                                        .trim_end_matches(" %")
                                        .parse::<f64>()
                                        .is_ok_and(|x| x > 5.0)
                            };
                            if (on("CLG1-C") || on("CLG-VLV")) && (on("HTG1-C") || on("HTG-VLV")) {
                                seen.push(format!("{} heating and cooling at once", d.name));
                            }
                        }
                    }
                    seen.sort();
                    seen.dedup();
                    if !seen.is_empty() {
                        problems += 1;
                        println!("{} {season:?} {day} {hour:02}:00", pkg.id);
                        for s in seen.iter().take(8) {
                            println!("    {s}");
                        }
                        if seen.len() > 8 {
                            println!("    … {} more", seen.len() - 8);
                        }
                    }
                }
            }
        }
    }
    println!("{problems} runs with something a BAS would flag");
}
