//! Bump tests on the school's loops, at their default tuning and with the
//! gains cranked: `cargo run --release -p bask-sim --example tuning`

use bask_points::PointValue;
use bask_sim::{Package, Season, Simulation};
use chrono::NaiveDate;

fn write(sim: &Simulation, device: &str, point: &str, v: f64) {
    let mut db = sim.db.lock();
    let d = db.devices.iter_mut().find(|d| d.name == device).unwrap();
    let o = d.find_by_name(point).unwrap();
    let prio = if d.objects[o].commandable {
        Some(8)
    } else {
        None
    };
    d.objects[o]
        .write_present_value(Some(PointValue::Real(v as f32)), prio)
        .unwrap();
}

fn read(sim: &Simulation, device: &str, point: &str) -> f64 {
    let db = sim.db.lock();
    let d = db.devices.iter().find(|d| d.name == device).unwrap();
    d.objects[d.find_by_name(point).unwrap()]
        .present_value
        .as_f64()
}

struct Bump {
    device: &'static str,
    pv: &'static str,
    sp: &'static str,
    /// Setpoint point to step and the new value.
    step: (&'static str, f64),
    hot: &'static [(&'static str, f64)],
}

/// Steps the setpoint, then samples every 5 s for 20 minutes:
/// (overshoot past the new setpoint as % of the step, peak-to-peak swing in
/// the last 10 minutes as % of the step, final error).
fn bump(sim: &mut Simulation, b: &Bump) -> (f64, f64, f64) {
    let before = read(sim, b.device, b.sp);
    write(sim, b.device, b.step.0, b.step.1);
    let mut samples = Vec::new();
    for _ in 0..240 {
        sim.advance(5.0 / sim.speed);
        samples.push((read(sim, b.device, b.pv), read(sim, b.device, b.sp)));
    }
    let (_, target) = *samples.last().unwrap();
    let step = (target - before).abs().max(1e-6);
    let up = target > before;
    let overshoot = samples
        .iter()
        .map(|(pv, _)| if up { pv - target } else { target - pv })
        .fold(0.0, f64::max)
        / step
        * 100.0;
    // how far it still swings once it should have settled
    let tail: Vec<f64> = samples[120..].iter().map(|(pv, sp)| pv - sp).collect();
    let swing = tail.iter().cloned().fold(f64::MIN, f64::max)
        - tail.iter().cloned().fold(f64::MAX, f64::min);
    let (pv, sp) = samples.last().unwrap();
    (overshoot, swing / step * 100.0, pv - sp)
}

/// RTU-3 (the conference room's oversized unit) for two summer hours: compressor starts in the
/// last hour, at default tuning and with the anti-short-cycle timer gone.
fn short_cycle(hot: bool) -> f64 {
    let pkg = Package::builtin()
        .into_iter()
        .find(|p| p.id == "small-office")
        .unwrap();
    let start = NaiveDate::from_ymd_opt(2026, 10, 27)
        .unwrap()
        .and_hms_opt(10, 0, 0)
        .unwrap();
    let mut sim = Simulation::new(pkg, start).unwrap();
    sim.set_season(Season::Summer);
    sim.speed = 60.0;
    if hot {
        write(&sim, "RTU-3", "CMP-MIN-ON", 0.0);
        write(&sim, "RTU-3", "CMP-MIN-OFF", 0.0);
        write(&sim, "RTU-3", "CLG-KP", 200.0);
    }
    for _ in 0..120 {
        sim.advance(1.0);
    }
    read(&sim, "RTU-3", "CMP-STARTS")
}

/// AHU-1's supply air error 20 minutes after a 3 °F setpoint step, with
/// the integral turned off.
fn p_only() -> f64 {
    let pkg = Package::builtin()
        .into_iter()
        .find(|p| p.id == "elementary-school")
        .unwrap();
    let start = NaiveDate::from_ymd_opt(2026, 10, 27)
        .unwrap()
        .and_hms_opt(9, 0, 0)
        .unwrap();
    let mut sim = Simulation::new(pkg, start).unwrap();
    sim.set_season(Season::Summer);
    sim.speed = 60.0;
    for _ in 0..120 {
        sim.advance(1.0);
    }
    write(&sim, "AHU-1", "SAT-TI", 0.0);
    write(&sim, "AHU-1", "SA-T-SP", 58.0);
    for _ in 0..20 {
        sim.advance(1.0);
    }
    read(&sim, "AHU-1", "SA-T") - read(&sim, "AHU-1", "SA-T-SP")
}

fn main() {
    println!(
        "RTU-3 compressor starts/hour: default {}, no anti-short-cycle + hot gain {}",
        short_cycle(false),
        short_cycle(true)
    );
    println!(
        "AHU-1 supply air offset after a 3 °F step with SAT-TI 0: {:+.2} °F",
        p_only()
    );
    let pkg = Package::builtin()
        .into_iter()
        .find(|p| p.id == "elementary-school")
        .unwrap();
    let start = NaiveDate::from_ymd_opt(2026, 10, 27)
        .unwrap()
        .and_hms_opt(9, 0, 0)
        .unwrap();
    let bumps = [
        Bump {
            device: "AHU-1",
            pv: "SA-T",
            sp: "SA-T-SP",
            step: ("SA-T-SP", 58.0),
            hot: &[("SAT-KP", 40.0), ("SAT-TI", 20.0)],
        },
        Bump {
            device: "AHU-1",
            pv: "DSP",
            sp: "DSP-SP",
            step: ("DSP-SP", 1.5),
            hot: &[("DSP-KP", 400.0), ("DSP-TI", 4.0)],
        },
        Bump {
            device: "VAV-104",
            pv: "AIRFLOW",
            sp: "AIRFLOW-SP",
            step: ("MIN-FLOW", 800.0),
            hot: &[("FLOW-KP", 6.0), ("FLOW-TI", 8.0)],
        },
    ];
    for b in &bumps {
        for hot in [false, true] {
            let mut sim = Simulation::new(pkg.clone(), start).unwrap();
            sim.set_season(Season::Summer);
            sim.speed = 60.0;
            for _ in 0..120 {
                sim.advance(1.0);
            }
            if hot {
                for (p, v) in b.hot {
                    write(&sim, b.device, p, *v);
                }
            }
            let (overshoot, swing, error) = bump(&mut sim, b);
            println!(
                "{:8} {:8} {:8} overshoot {overshoot:6.1} %   still swinging {swing:6.1} %   final error {error:7.2}",
                b.device,
                b.pv,
                if hot { "hot" } else { "default" },
            );
        }
    }
}
