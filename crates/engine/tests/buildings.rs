//! Each built-in building runs a day sensibly, and the scenarios and BAS
//! overrides do what they say.

use bask_points::{PointValue, Reliability};
use bask_sim::scenario::Resolved;
use bask_sim::{Package, Season, Simulation, Span};
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
    // static follows its setpoint, which the reset has brought well under design
    let dsp = read(&sim, "AHU-1", "DSP").as_f64();
    let sp = read(&sim, "AHU-1", "DSP-SP").as_f64();
    assert!((dsp - sp).abs() < 0.15, "static at {dsp} for {sp}");
    assert!(sp < 1.0, "static reset to {sp}");
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
            (flow - sp).abs() < sp * 0.15 + 30.0,
            "{}: {flow} cfm for {sp}",
            d.name
        );
    }
}

#[test]
fn trim_and_respond_follows_the_neediest_box() {
    let mut sim = building("elementary-school", Season::Mild, 9);
    run(&mut sim, 120);
    // cool out: the supply air rides up as far as the zones let it
    let sat_sp = read(&sim, "AHU-1", "SA-T-SP").as_f64();
    assert!(sat_sp > 57.0, "supply air reset to {sat_sp}");
    write(
        &sim,
        "AHU-1",
        "SAT-RESET",
        Some(PointValue::Binary(false)),
        8,
    );
    run(&mut sim, 1);
    assert_eq!(read(&sim, "AHU-1", "SA-T-SP").as_f64(), 55.0, "reset off");
    write(&sim, "AHU-1", "SAT-RESET", None, 8);

    // a box that can't get its air pushes static to the top...
    let before = read(&sim, "AHU-1", "DSP-SP").as_f64();
    let vav = sim.device_index("VAV-101").unwrap();
    let stuck = sim
        .quick_faults(vav)
        .into_iter()
        .find(|f| f.id.ends_with(":damper"))
        .unwrap();
    sim.start(stuck);
    run(&mut sim, 30);
    assert!(read(&sim, "VAV-101", "PRESS-REQ").as_f64() >= 2.0);
    let pushed = read(&sim, "AHU-1", "DSP-SP").as_f64();
    assert!(pushed > before + 0.4, "static {before} -> {pushed}");
    // ...until it's told to stop counting
    write(
        &sim,
        "VAV-101",
        "REQ-IMPORTANCE",
        Some(PointValue::Real(0.0)),
        16,
    );
    run(&mut sim, 30);
    let trimmed = read(&sim, "AHU-1", "DSP-SP").as_f64();
    assert!(trimmed < pushed - 0.3, "static {pushed} -> {trimmed}");
}

#[test]
fn co2_drives_ventilation() {
    let mut sim = building("elementary-school", Season::Mild, 6);
    run(&mut sim, 60);
    // empty classroom: the box sits at its area minimum
    let min = read(&sim, "VAV-101", "MIN-FLOW").as_f64();
    let empty = read(&sim, "VAV-101", "EFF-MIN-FLOW").as_f64();
    assert!(empty < min * 0.5, "{empty} of {min} cfm");
    run(&mut sim, 150);
    let full = read(&sim, "VAV-101", "EFF-MIN-FLOW").as_f64();
    assert!(
        full > empty + (min - empty) * 0.5,
        "class in: {full} of {min} cfm"
    );
    // turn it off and the full minimum comes back regardless
    write(&sim, "RTU-GYM", "DCV", Some(PointValue::Binary(false)), 8);
    run(&mut sim, 1);
    assert_eq!(
        read(&sim, "RTU-GYM", "EFF-MIN-OA").as_f64(),
        read(&sim, "RTU-GYM", "MIN-OA").as_f64()
    );
}

#[test]
fn humidity_follows_the_coil() {
    // summer: the coil wrings the air out; warmer supply air leaves it damper
    let mut sim = building("elementary-school", Season::Summer, 7);
    run(&mut sim, 240);
    let rh = |sim: &Simulation| {
        let z = sim.snapshot();
        z.zones.iter().map(|z| z.rh).sum::<f64>() / z.zones.len() as f64
    };
    let dry = rh(&sim);
    assert!((40.0..65.0).contains(&dry), "rooms at {dry:.0} %");
    write(&sim, "AHU-1", "SA-T-SP", Some(PointValue::Real(63.0)), 8);
    run(&mut sim, 120);
    let damp = rh(&sim);
    assert!(damp > dry + 4.0, "{dry:.0} % -> {damp:.0} %");
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

#[test]
fn trends_record_every_controller() {
    let mut sim = building("elementary-school", Season::Mild, 9);
    run(&mut sim, 20);
    let ahu = sim.device_index("AHU-1").unwrap();
    let titles = sim.chart_titles(ahu);
    assert!(titles.contains(&"Supply air") && titles.contains(&"Duct static"));
    let i = titles.iter().position(|t| *t == "Supply air").unwrap();
    let t = sim.trend(ahu, i, Span::Minutes15).unwrap();
    let sat = &t.lines[0];
    assert_eq!(sat.name, "SA-T");
    assert!(sat.points.len() > 400, "{} samples", sat.points.len());
    assert!(sat.points[0].0 >= -900.0);
    let (ago, last) = *sat.points.last().unwrap();
    assert!(ago > -3.0);
    assert!((last as f64 - read(&sim, "AHU-1", "SA-T").as_f64()).abs() < 0.5);
    // the day chart reaches back through the hours the sim settled for
    let day = sim.trend(ahu, i, Span::Day).unwrap();
    assert!(day.lines[0].points.len() > 150);
    // every chart on every controller has something to draw
    for d in 0..sim.snapshot().devices.len() {
        for c in 0..sim.chart_titles(d).len() {
            let t = sim.trend(d, c, Span::Hour).unwrap();
            assert!(t.lines.iter().all(|l| !l.points.is_empty()), "{}", t.title);
        }
    }
}

#[test]
fn every_lesson_sets_up_and_names_real_points() {
    for pkg in Package::builtin() {
        let names: Vec<String> = {
            let sim = building(&pkg.id, Season::Mild, 6);
            let db = sim.db.lock();
            db.devices
                .iter()
                .flat_map(|d| {
                    std::iter::once(d.name.clone()).chain(d.objects.iter().map(|o| o.name.clone()))
                })
                .collect()
        };
        for lesson in &pkg.lessons {
            let who = format!("{} {}", pkg.id, lesson.id);
            let text = lesson.steps.join(" ") + " " + &lesson.expect + " " + &lesson.goal;
            for (i, quoted) in text.split('`').enumerate() {
                if i % 2 == 1 {
                    assert!(
                        names.iter().any(|n| n == quoted),
                        "{who}: no point {quoted}"
                    );
                }
            }
            let mut sim = building(&pkg.id, Season::Mild, 6);
            sim.start_lesson(&lesson.id).unwrap();
            if let Some(device) = &lesson.setup.device {
                let d = sim.device_index(device).unwrap();
                if let Some(chart) = &lesson.setup.chart {
                    assert!(
                        sim.chart_titles(d).contains(&chart.as_str()),
                        "{who}: {device} has no {chart} chart"
                    );
                }
            }
            for id in &lesson.setup.scenarios {
                assert!(sim.is_running(id), "{who}: {id} running");
            }
            run(&mut sim, 5);
        }
    }
}

#[test]
fn the_bacnet_schedule_runs_the_building() {
    let mut sim = building("elementary-school", Season::Mild, 5);
    // the controllers keep the sim's time, not the computer's
    assert_eq!(sim.db.lock().now, Some(sim.now));
    let snap = sim.snapshot();
    assert_eq!(snap.schedule, snap.hours, "built from the package's hours");
    // a BAS closes the building on Tuesdays
    {
        let mut db = sim.db.lock();
        let site = db.local_device().unwrap();
        let o = db.devices[site].find_by_name("OCC-SCHEDULE").unwrap();
        db.devices[site].objects[o]
            .as_schedule_mut()
            .unwrap()
            .weekly[1]
            .clear();
    }
    run(&mut sim, 7 * 60);
    let snap = sim.snapshot();
    assert!(!snap.occupied, "schedule off: unoccupied at noon");
    assert_eq!(snap.schedule, None);
    // putting the building back as built brings the schedule back
    sim.clear_overrides();
    run(&mut sim, 1);
    assert!(sim.snapshot().occupied);
}

#[test]
fn alarms_go_out_as_bacnet_events_and_trends_fill() {
    let mut sim = building("elementary-school", Season::Mild, 9);
    let ahu = sim.device_index("AHU-1").unwrap();
    let fan = sim
        .quick_faults(ahu)
        .into_iter()
        .find(|f| f.id.ends_with(":fan"))
        .unwrap();
    sim.start(fan);
    run(&mut sim, 2);
    let (device, instance) = {
        let db = sim.db.lock();
        let d = &db.devices[ahu];
        (
            d.instance,
            d.objects[d.find_by_name("SF-FAIL").unwrap()].instance,
        )
    };
    let notices = sim.db.lock().take_notices();
    let sf_fail = notices
        .iter()
        .find(|n| n.device == device && n.instance == instance)
        .expect("SF-FAIL went out as an event");
    assert_eq!(sf_fail.to, bask_points::EventState::Offnormal);
    assert!(read(&sim, "AHU-1", "SF-FAIL").as_bool());
    // a failed zone sensor reports a fault
    let vav = sim.device_index("VAV-101").unwrap();
    let sensor = sim
        .quick_faults(vav)
        .into_iter()
        .find(|f| f.id.ends_with(":zn-t"))
        .unwrap();
    sim.start(sensor);
    run(&mut sim, 2);
    let notices = sim.db.lock().take_notices();
    assert!(
        notices
            .iter()
            .any(|n| n.to == bask_points::EventState::Fault),
        "{notices:?}"
    );
    // each controller keeps a trend log of its zone, a sample a minute
    let db = sim.db.lock();
    let d = &db.devices[vav];
    let tl = d.objects[d.find_by_name("TL-ZN-T").unwrap()]
        .as_trend_log()
        .unwrap();
    assert!(tl.records().count() > 180, "{}", tl.records().count());
}
