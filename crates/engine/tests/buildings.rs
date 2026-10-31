//! Each built-in building runs a day sensibly, and the scenarios and BAS
//! overrides do what they say.

use bask_points::{PointValue, Reliability};
use bask_sim::scenario::Resolved;
use bask_sim::{Package, Season, Simulation};
use chrono::{NaiveDate, NaiveDateTime};

/// A Tuesday, before anyone's in.
fn tuesday(hour: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 10, 27)
        .unwrap()
        .and_hms_opt(hour, 0, 0)
        .unwrap()
}

fn building(id: &str, season: Season, hour: u32) -> Simulation {
    let pkg = Package::builtin().into_iter().find(|p| p.id == id).unwrap();
    let mut sim = Simulation::new(pkg, tuesday(hour)).unwrap();
    sim.set_season(season);
    sim
}

/// Run `minutes` of sim time in one-minute chunks.
fn run(sim: &mut Simulation, minutes: u32) {
    sim.speed = 60.0;
    for _ in 0..minutes {
        sim.advance(1.0);
    }
}

fn read(sim: &Simulation, device: &str, point: &str) -> PointValue {
    let db = sim.db.lock();
    let d = db.devices.iter().find(|d| d.name == device).unwrap();
    d.objects[d.find_by_name(point).unwrap()].present_value
}

fn write(sim: &Simulation, device: &str, point: &str, value: Option<PointValue>, priority: u8) {
    let mut db = sim.db.lock();
    let d = db.devices.iter_mut().find(|d| d.name == device).unwrap();
    let o = d.find_by_name(point).unwrap();
    d.objects[o]
        .write_present_value(value, Some(priority))
        .unwrap();
}

fn zone_temp(sim: &Simulation, name: &str) -> f64 {
    sim.zones.iter().find(|z| z.name == name).unwrap().temp
}

#[test]
fn every_building_holds_its_setpoints_through_the_day() {
    for pkg in Package::builtin() {
        for season in Season::ALL {
            let mut sim = building(&pkg.id, season, 5);
            run(&mut sim, 7 * 60); // to noon
            let snap = sim.snapshot();
            assert!(snap.occupied, "{} {season:?}: occupied at noon", pkg.id);
            for z in &snap.zones {
                assert!(
                    z.temp.is_finite() && z.co2.is_finite(),
                    "{} {}: not finite",
                    pkg.id,
                    z.name
                );
                // the warehouse floor only has heat
                let Some((clg, htg)) = z.setpoints else {
                    continue;
                };
                let hi = if clg >= 95.0 { 100.0 } else { clg + 2.5 };
                assert!(
                    z.temp > htg - 2.5 && z.temp < hi,
                    "{} {season:?}: {} at {:.1} °F, band {htg}–{clg}",
                    pkg.id,
                    z.name,
                    z.temp
                );
            }
            run(&mut sim, 14 * 60); // through the night
            for z in &sim.snapshot().zones {
                assert!(
                    (35.0..105.0).contains(&z.temp),
                    "{} {season:?}: {} at {:.1} overnight",
                    pkg.id,
                    z.name,
                    z.temp
                );
            }
        }
    }
}

#[test]
fn every_scenario_resolves_and_runs() {
    for pkg in Package::builtin() {
        let mut sim = building(&pkg.id, Season::Mild, 8);
        let ids: Vec<String> = sim.catalog.iter().map(|s| s.id.clone()).collect();
        for id in &ids {
            sim.start_scenario(id).unwrap();
            let a = sim.active.last().unwrap();
            for r in &a.resolved {
                match r {
                    Resolved::Point(None) => {
                        panic!("{}: scenario {id} names a point that doesn't exist", pkg.id)
                    }
                    Resolved::Devices(d) if d.is_empty() => {
                        panic!("{}: scenario {id} names no device", pkg.id)
                    }
                    _ => {}
                }
            }
            run(&mut sim, 5);
            sim.stop_scenario(id);
        }
        // quick faults too
        for d in 0..sim.snapshot().devices.len() {
            for f in sim.quick_faults(d) {
                sim.start(f.clone());
                run(&mut sim, 1);
                sim.stop_scenario(&f.id);
            }
        }
        run(&mut sim, 120);
        assert!(
            sim.active.is_empty(),
            "{}: everything stopped fades out",
            pkg.id
        );
    }
}

#[test]
fn fan_failure_alarms_and_the_zone_drifts() {
    let mut sim = building("small-office", Season::Summer, 9);
    run(&mut sim, 120);
    let before = zone_temp(&sim, "Open Office");
    sim.start_scenario("rtu1-fan-belt").unwrap();
    run(&mut sim, 1);
    assert_eq!(
        read(&sim, "RTU-1", "SF-C"),
        PointValue::Binary(true),
        "still commanded on"
    );
    assert_eq!(read(&sim, "RTU-1", "SF-S"), PointValue::Binary(false));
    assert_eq!(read(&sim, "RTU-1", "SF-FAIL"), PointValue::Binary(true));
    run(&mut sim, 90);
    assert!(
        zone_temp(&sim, "Open Office") > before + 2.0,
        "open office warms with no air"
    );
    assert!(
        sim.log.iter().any(|e| e.text.contains("SF-FAIL")),
        "alarm logged"
    );
}

#[test]
fn chiller_trip_warms_the_loop() {
    let mut sim = building("elementary-school", Season::Summer, 9);
    run(&mut sim, 180);
    let chws = read(&sim, "CH-1", "CHWS-T").as_f64();
    assert!(chws < 47.0, "chiller holding setpoint, got {chws}");
    sim.start_scenario("chiller-trip").unwrap();
    run(&mut sim, 60);
    assert_eq!(read(&sim, "CH-1", "CH-ALM"), PointValue::Binary(true));
    assert!(read(&sim, "CH-1", "CHWS-T").as_f64() > chws + 5.0);
    assert!(read(&sim, "AHU-1", "SA-T").as_f64() > 57.0);
}

#[test]
fn bas_overrides_win_and_relinquish() {
    let mut sim = building("small-office", Season::Mild, 9);
    run(&mut sim, 60);
    write(&sim, "RTU-2", "OCC-HTG-SP", Some(PointValue::Real(76.0)), 8);
    write(&sim, "RTU-2", "OCC-CLG-SP", Some(PointValue::Real(78.0)), 8);
    run(&mut sim, 120);
    assert!(
        zone_temp(&sim, "Private Offices") > 75.0,
        "heats to the BAS setpoint"
    );
    // a manual fan override at priority 8 turns the fan off whatever the program says
    write(&sim, "RTU-2", "SF-C", Some(PointValue::Binary(false)), 8);
    run(&mut sim, 1);
    assert_eq!(read(&sim, "RTU-2", "SF-S"), PointValue::Binary(false));
    write(&sim, "RTU-2", "SF-C", None, 8);
    run(&mut sim, 1);
    assert_eq!(read(&sim, "RTU-2", "SF-S"), PointValue::Binary(true));
    // building unoccupied from the site controller
    write(
        &sim,
        "SITE-CTRL",
        "BLDG-OCC-MODE",
        Some(PointValue::Multi(3)),
        8,
    );
    run(&mut sim, 1);
    assert_eq!(read(&sim, "RTU-1", "OCC"), PointValue::Binary(false));
}

#[test]
fn out_of_service_sensor_drives_the_controller() {
    let mut sim = building("small-office", Season::Mild, 9);
    run(&mut sim, 60);
    {
        let mut db = sim.db.lock();
        let d = db.devices.iter_mut().find(|d| d.name == "RTU-3").unwrap();
        let o = d.find_by_name("ZN-T").unwrap();
        d.objects[o].out_of_service = true;
        d.objects[o]
            .write_present_value(Some(PointValue::Real(85.0)), None)
            .unwrap();
    }
    run(&mut sim, 15);
    assert_eq!(
        read(&sim, "RTU-3", "ZN-T"),
        PointValue::Real(85.0),
        "the sim leaves it alone"
    );
    assert!(
        read(&sim, "RTU-3", "CLG-DMD").as_f64() > 50.0,
        "and the unit cools to it"
    );
}

#[test]
fn sensor_failure_reports_a_fault() {
    let mut sim = building("small-office", Season::Mild, 9);
    sim.start_scenario("lobby-sensor-fail").unwrap();
    run(&mut sim, 1);
    let db = sim.db.lock();
    let d = db.devices.iter().find(|d| d.name == "RTU-4").unwrap();
    let o = &d.objects[d.find_by_name("ZN-T").unwrap()];
    assert_eq!(o.present_value, PointValue::Real(-40.0));
    assert_eq!(o.reliability, Reliability::OpenLoop);
}

#[test]
fn offline_and_power_outage_take_devices_off_the_network() {
    let mut sim = building("small-office", Season::Mild, 9);
    sim.start_scenario("rtu3-offline").unwrap();
    run(&mut sim, 1);
    let online = |sim: &Simulation, name: &str| {
        sim.db
            .lock()
            .devices
            .iter()
            .find(|d| d.name == name)
            .unwrap()
            .online
    };
    assert!(!online(&sim, "RTU-3"));
    assert!(online(&sim, "RTU-1"));
    run(&mut sim, 31);
    assert!(online(&sim, "RTU-3"), "back after its 30 minutes");
    sim.start_scenario("power-outage").unwrap();
    run(&mut sim, 1);
    assert!(sim.db.lock().devices.iter().all(|d| !d.online));
    assert_eq!(sim.snapshot().kw, 0.0);
}

#[test]
fn ahu_holds_duct_static() {
    let mut sim = building("elementary-school", Season::Summer, 7);
    run(&mut sim, 240);
    let dsp = read(&sim, "AHU-1", "DSP").as_f64();
    assert!((dsp - 1.25).abs() < 0.1, "static at {dsp}");
    // every box gets its air
    let snap = sim.snapshot();
    for d in snap.devices.iter().filter(|d| d.name.starts_with("VAV-")) {
        let get = |n: &str| d.points.iter().find(|p| p.name == n).unwrap().value.clone();
        let flow: f64 = get("AIRFLOW")
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let sp: f64 = get("AIRFLOW-SP")
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            (flow - sp).abs() < sp * 0.1 + 20.0,
            "{}: {flow} cfm for {sp}",
            d.name
        );
    }
}

/// Bump test on AHU-1's supply air loop: (overshoot, swing in the last 10
/// minutes), both as a share of the 3 °F step.
fn sat_bump(hot: bool) -> (f64, f64) {
    let mut sim = building("elementary-school", Season::Summer, 9);
    run(&mut sim, 120);
    if hot {
        let mut db = sim.db.lock();
        let d = db.devices.iter_mut().find(|d| d.name == "AHU-1").unwrap();
        for (p, v) in [("SAT-KP", 40.0), ("SAT-TI", 20.0)] {
            let o = d.find_by_name(p).unwrap();
            d.objects[o]
                .write_present_value(Some(PointValue::Real(v)), None)
                .unwrap();
        }
    }
    write(&sim, "AHU-1", "SA-T-SP", Some(PointValue::Real(58.0)), 8);
    let mut errors = Vec::new();
    for _ in 0..240 {
        sim.advance(5.0 / sim.speed);
        errors.push(read(&sim, "AHU-1", "SA-T").as_f64() - 58.0);
    }
    let overshoot = errors.iter().cloned().fold(0.0, f64::max) / 3.0;
    let tail = &errors[120..];
    let swing = (tail.iter().cloned().fold(f64::MIN, f64::max)
        - tail.iter().cloned().fold(f64::MAX, f64::min))
        / 3.0;
    (overshoot, swing)
}

#[test]
fn default_tuning_is_calm_and_hot_tuning_hunts() {
    let (overshoot, swing) = sat_bump(false);
    assert!(
        overshoot < 0.15 && swing < 0.1,
        "default: overshoot {overshoot:.2}, swing {swing:.2}"
    );
    let (overshoot, swing) = sat_bump(true);
    assert!(
        overshoot > 1.0 && swing > 1.0,
        "hot gains should hunt: overshoot {overshoot:.2}, swing {swing:.2}"
    );
}

#[test]
fn mystery_faults_hide_what_broke() {
    let mut sim = building("elementary-school", Season::Mild, 9);
    let id = sim.start_mystery();
    let answer = sim.mystery_answer(&id).unwrap().to_string();
    let snap = sim.snapshot();
    let s = snap.scenarios.iter().find(|s| s.id == id).unwrap();
    assert!(s.active && s.name.starts_with("Mystery fault"));
    assert!(!s.name.contains(&answer) && !s.description.contains(&answer));
    assert!(
        !sim.log.iter().any(|e| e.text.contains(&answer)),
        "the log doesn't give it away"
    );
    run(&mut sim, 5);
    sim.stop_scenario(&id);
}

/// Left alone, every building runs normally: nothing in alarm or fault, and
/// every conditioned zone in its band whenever people are due in (optimal
/// start has it ready by opening).
#[test]
fn left_alone_everything_runs_normally() {
    for pkg in Package::builtin() {
        for season in [Season::Winter, Season::Summer] {
            let mut sim = building(&pkg.id, season, 3);
            for minute in 0..14 * 60 {
                run(&mut sim, 1);
                if minute % 15 != 0 {
                    continue;
                }
                let snap = sim.snapshot();
                for d in &snap.devices {
                    if let Some(p) = d.points.iter().find(|p| p.alarm || p.fault) {
                        panic!(
                            "{} {season:?} {}: {} {} = {}",
                            pkg.id, snap.now, d.name, p.name, p.value
                        );
                    }
                }
                let h = bask_sim::world::hour_of(snap.now);
                if snap
                    .hours
                    .is_some_and(|(open, close)| h >= open && h < close)
                {
                    for z in snap
                        .zones
                        .iter()
                        .filter(|z| z.setpoints.is_some_and(|(clg, _)| clg < 95.0))
                    {
                        assert!(
                            z.off_band().abs() <= 2.0,
                            "{} {season:?} {}: {} at {:.1}",
                            pkg.id,
                            snap.now,
                            z.name,
                            z.temp
                        );
                    }
                }
            }
            assert!(sim.active.is_empty(), "nothing runs unless fired");
        }
    }
}
