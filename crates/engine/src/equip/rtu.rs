//! Packaged single-zone rooftop unit: constant-volume fan, staged DX
//! cooling, staged gas or electric heat, and an economizer that cools first
//! when the outside air is good for it.

use super::*;
use crate::control::{Delay, Pi, Stager, Stroke, lag};
use crate::package::{Heating, RtuDef};
use crate::scenario::Actuator;
use crate::zone::Zone;
use std::collections::VecDeque;

const MODES: [&str; 6] = [
    "Off",
    "Fan Only",
    "Cooling",
    "Economizer",
    "Econ + Cooling",
    "Heating",
];

struct Points {
    zn_t: PointRef,
    zn_co2: PointRef,
    sa_t: PointRef,
    ra_t: PointRef,
    ma_t: PointRef,
    oa_t: PointRef,
    sf_c: PointRef,
    sf_s: PointRef,
    clg: Vec<PointRef>,
    htg: Vec<PointRef>,
    oad: PointRef,
    occ_clg: PointRef,
    occ_htg: PointRef,
    unocc_clg: PointRef,
    unocc_htg: PointRef,
    min_oa: PointRef,
    eff_clg: PointRef,
    eff_htg: PointRef,
    clg_dmd: PointRef,
    htg_dmd: PointRef,
    occ_mode: PointRef,
    occ: PointRef,
    mode: PointRef,
    flt: PointRef,
    kw: PointRef,
    clg_tuning: Tuning,
    htg_tuning: Tuning,
    min_on: PointRef,
    min_off: PointRef,
    starts: PointRef,
    sf_fail: PointRef,
    clg_fail: PointRef,
    htg_fail: PointRef,
}

pub struct Rtu {
    pub dev: usize,
    pub id: String,
    pub zone: usize,
    cfm: f64,
    tons: f64,
    heating: Heating,
    heat_mbh: f64,
    economizer: bool,
    p: Points,
    clg_pi: Pi,
    htg_pi: Pi,
    cool: Stager,
    heat: Stager,
    oad: Stroke,
    /// Sim seconds since start, and when each compressor start happened.
    clock_s: f64,
    start_times: VecDeque<f64>,
    counted_starts: u64,
    zone_reading: Option<f64>,
    unocc_hold_s: f64,
    sf_delay: Delay,
    clg_delay: Delay,
    htg_delay: Delay,
    filter_load: f64,
    sat: f64,
    mat: f64,
    pub fan_running: bool,
    pub occupied: bool,
    pub mode: u32,
    pub stages_cool: usize,
    pub stages_heat: usize,
    pub kw: f64,
}

impl Rtu {
    pub fn new(def: &RtuDef, zone: usize, b: &mut DevBuilder) -> Self {
        let heats = def.heating != Heating::None;
        let p = Points {
            zn_t: b.ai("ZN-T", "Zone temperature", Units::DegreesFahrenheit, 72.0),
            zn_co2: b.ai("ZN-CO2", "Zone CO2", Units::PartsPerMillion, 450.0),
            sa_t: b.ai(
                "SA-T",
                "Supply air temperature",
                Units::DegreesFahrenheit,
                70.0,
            ),
            ra_t: b.ai(
                "RA-T",
                "Return air temperature",
                Units::DegreesFahrenheit,
                72.0,
            ),
            ma_t: b.ai(
                "MA-T",
                "Mixed air temperature",
                Units::DegreesFahrenheit,
                70.0,
            ),
            oa_t: b.ai(
                "OA-T",
                "Outside air temperature (unit sensor)",
                Units::DegreesFahrenheit,
                60.0,
            ),
            sf_c: b.bo("SF-C", "Supply fan command", ["Off", "On"]),
            sf_s: b.bi("SF-S", "Supply fan status", ["Off", "On"]),
            clg: (1..=def.cool_stages.max(1))
                .map(|i| {
                    b.bo(
                        &format!("CLG{i}-C"),
                        &format!("Compressor stage {i} command"),
                        ["Off", "On"],
                    )
                })
                .collect(),
            htg: if heats {
                let what = if def.heating == Heating::Electric {
                    "Electric heat"
                } else {
                    "Gas heat"
                };
                (1..=def.heat_stages.max(1))
                    .map(|i| {
                        b.bo(
                            &format!("HTG{i}-C"),
                            &format!("{what} stage {i} command"),
                            ["Off", "On"],
                        )
                    })
                    .collect()
            } else {
                Vec::new()
            },
            oad: b.ao("OAD-O", "Outside air damper command", Units::Percent, 0.0),
            occ_clg: b.sp(
                "OCC-CLG-SP",
                "Occupied cooling setpoint",
                Units::DegreesFahrenheit,
                74.0,
                60.0,
                90.0,
            ),
            occ_htg: b.sp(
                "OCC-HTG-SP",
                "Occupied heating setpoint",
                Units::DegreesFahrenheit,
                70.0,
                50.0,
                85.0,
            ),
            unocc_clg: b.sp(
                "UNOCC-CLG-SP",
                "Unoccupied cooling setpoint",
                Units::DegreesFahrenheit,
                85.0,
                60.0,
                95.0,
            ),
            unocc_htg: b.sp(
                "UNOCC-HTG-SP",
                "Unoccupied heating setpoint",
                Units::DegreesFahrenheit,
                60.0,
                40.0,
                80.0,
            ),
            min_oa: b.sp(
                "MIN-OA",
                "Minimum outside air damper position",
                Units::Percent,
                def.min_oa_pct,
                0.0,
                100.0,
            ),
            eff_clg: b.av(
                "EFF-CLG-SP",
                "Effective cooling setpoint",
                Units::DegreesFahrenheit,
                74.0,
            ),
            eff_htg: b.av(
                "EFF-HTG-SP",
                "Effective heating setpoint",
                Units::DegreesFahrenheit,
                70.0,
            ),
            clg_dmd: b.av("CLG-DMD", "Cooling demand", Units::Percent, 0.0),
            htg_dmd: b.av("HTG-DMD", "Heating demand", Units::Percent, 0.0),
            occ_mode: b.occ_mode(),
            occ: b.occ_status(),
            mode: b.msv("UNIT-MODE", "Unit mode", &MODES, 1, false),
            flt: b.bi(
                "FLT-S",
                "Filter status (differential pressure switch)",
                ["Clean", "Dirty"],
            ),
            kw: b.av("KW", "Unit power", Units::Kilowatts, 0.0),
            clg_tuning: b.tuning("CLG", "Cooling", "°F", 40.0, 1200.0, 500.0),
            htg_tuning: b.tuning("HTG", "Heating", "°F", 40.0, 1200.0, 500.0),
            min_on: b.param(
                "CMP-MIN-ON",
                "Compressor minimum on time",
                Units::Seconds,
                180.0,
                0.0,
                1800.0,
            ),
            min_off: b.param(
                "CMP-MIN-OFF",
                "Compressor minimum off time (anti-short-cycle)",
                Units::Seconds,
                300.0,
                0.0,
                1800.0,
            ),
            starts: b.av(
                "CMP-STARTS",
                "Compressor starts in the last hour (over 6 is short cycling)",
                Units::NoUnits,
                0.0,
            ),
            sf_fail: b.alarm("SF-FAIL", "Supply fan failure: commanded on, no status"),
            clg_fail: b.alarm("CMP-ALM", "Compressor lockout"),
            htg_fail: b.alarm("HTG-ALM", "Heating failure"),
        };
        Rtu {
            dev: b.device(),
            id: def.common.id.clone(),
            zone,
            cfm: def.supply_cfm,
            tons: def.cool_tons,
            heating: def.heating,
            heat_mbh: def.heat_mbh,
            economizer: def.economizer,
            clg_pi: Pi::new(40.0, 1200.0, 0.0, 100.0),
            htg_pi: Pi::new(40.0, 1200.0, 0.0, 100.0),
            cool: Stager::new(def.cool_stages, 180.0, 300.0),
            heat: Stager::new(if heats { def.heat_stages } else { 1 }, 120.0, 180.0),
            oad: Stroke::new(90.0),
            clock_s: 0.0,
            start_times: VecDeque::new(),
            counted_starts: 0,
            zone_reading: None,
            p,
            unocc_hold_s: 0.0,
            sf_delay: Delay::default(),
            clg_delay: Delay::default(),
            htg_delay: Delay::default(),
            filter_load: 0.15,
            sat: 70.0,
            mat: 70.0,
            fan_running: false,
            occupied: false,
            mode: 1,
            stages_cool: 0,
            stages_heat: 0,
            kw: 0.0,
        }
    }

    pub fn step(&mut self, env: &Env, db: &mut PointDb, zones: &[Zone], bus: &mut Bus) {
        let (dt, m, p) = (env.dt, env.mods, &self.p);
        let zone = &zones[self.zone];
        let oat = env.weather.oat;
        let dead = m.shutdown();

        // a wall sensor trails the room by a minute or two
        let zr = lag(self.zone_reading.unwrap_or(zone.temp), zone.temp, 90.0, dt);
        self.zone_reading = Some(zr);
        sense(db, m, p.zn_t, zr);
        sense(db, m, p.zn_co2, zone.co2);
        sense(db, m, p.oa_t, oat);

        self.occupied = occupied(db.read_u32(p.occ_mode), env.occupied);
        db.set_bool(p.occ, self.occupied);
        let (csp, hsp) = if self.occupied {
            (db.read_f64(p.occ_clg), db.read_f64(p.occ_htg))
        } else {
            (db.read_f64(p.unocc_clg), db.read_f64(p.unocc_htg))
        };
        db.set_real(p.eff_clg, csp);
        db.set_real(p.eff_htg, hsp);

        let zt = db.read_f64(p.zn_t);
        p.clg_tuning.apply(db, &mut self.clg_pi);
        p.htg_tuning.apply(db, &mut self.htg_pi);
        let clg = self.clg_pi.update(zt - csp, dt);
        let htg = if self.p.htg.is_empty() {
            0.0
        } else {
            self.htg_pi.update(hsp - zt, dt)
        };
        db.set_real(p.clg_dmd, clg);
        db.set_real(p.htg_dmd, htg);

        // fan: on while occupied; cycles with demand while unoccupied
        self.unocc_hold_s = if clg > 15.0 || htg > 15.0 {
            600.0
        } else {
            (self.unocc_hold_s - dt).max(0.0)
        };
        let fan_cmd = program_bool(db, p.sf_c, self.occupied || self.unocc_hold_s > 0.0);
        self.fan_running = fan_cmd && !m.fail_fan[self.dev] && !dead;
        status(db, p.sf_s, self.fan_running);
        let sf_alarm = self
            .sf_delay
            .update(fan_cmd && !self.fan_running && !dead, 30.0, dt);
        alarm(db, p.sf_fail, sf_alarm);

        // economizer cools first, compressors pick up the rest
        let rat = zone.temp + 0.8;
        let min_oa = if self.occupied {
            db.read_f64(p.min_oa)
        } else {
            0.0
        };
        let econ = self.economizer && self.fan_running && oat < 65.0 && oat < rat - 1.0;
        let (mut oad, mut mech) = (min_oa, clg);
        if econ && clg > 0.0 {
            oad = min_oa + (100.0 - min_oa) * (clg / 50.0).min(1.0);
            mech = ((clg - 50.0) * 2.0).max(0.0);
        }
        if !self.fan_running {
            oad = 0.0;
        }
        let oad_cmd = program_real(db, p.oad, oad);

        self.cool.min_on_s = db.read_f64(p.min_on);
        self.cool.min_off_s = db.read_f64(p.min_off);
        self.cool
            .update(mech, self.fan_running && oat > 40.0 && htg <= 0.0, dt);
        self.clock_s += dt;
        for _ in self.counted_starts..self.cool.starts {
            self.start_times.push_back(self.clock_s);
        }
        self.counted_starts = self.cool.starts;
        while self
            .start_times
            .front()
            .is_some_and(|t| self.clock_s - t > 3600.0)
        {
            self.start_times.pop_front();
        }
        db.set_real(p.starts, self.start_times.len() as f64);
        self.heat.update(htg, self.fan_running && clg <= 0.0, dt);
        let clg_cmds: Vec<bool> = p
            .clg
            .iter()
            .enumerate()
            .map(|(i, r)| program_bool(db, *r, self.cool.on(i)))
            .collect();
        let htg_cmds: Vec<bool> = p
            .htg
            .iter()
            .enumerate()
            .map(|(i, r)| program_bool(db, *r, self.heat.on(i)))
            .collect();
        let calls_cool = clg_cmds.iter().any(|c| *c);
        let calls_heat = htg_cmds.iter().any(|c| *c);
        let can_cool = self.fan_running && !m.fail_cool[self.dev];
        let can_heat = self.fan_running && !m.fail_heat[self.dev];
        self.stages_cool = if can_cool {
            clg_cmds.iter().filter(|c| **c).count()
        } else {
            0
        };
        self.stages_heat = if can_heat {
            htg_cmds.iter().filter(|c| **c).count()
        } else {
            0
        };
        let a = self
            .clg_delay
            .update(calls_cool && m.fail_cool[self.dev] && !dead, 60.0, dt);
        alarm(db, p.clg_fail, a);
        let a = self
            .htg_delay
            .update(calls_heat && m.fail_heat[self.dev] && !dead, 90.0, dt);
        alarm(db, p.htg_fail, a);

        // physics
        let cfm = if self.fan_running { self.cfm } else { 0.0 };
        // spring return: the damper closes when the fan stops
        let oad_pos = self
            .oad
            .update(if self.fan_running { oad_cmd } else { 0.0 }, dt);
        let oad_actual = m.stuck(self.dev, Actuator::OaDamper).unwrap_or(oad_pos);
        let oaf = (oad_actual / 100.0).clamp(0.0, 1.0);
        let mat_target = if self.fan_running {
            oaf * oat + (1.0 - oaf) * rat
        } else {
            0.8 * rat + 0.2 * oat
        };
        self.mat = lag(
            self.mat,
            mat_target,
            if self.fan_running { 30.0 } else { 600.0 },
            dt,
        );
        let capacity = (1.0 - 0.006 * (oat - 95.0)).clamp(0.7, 1.15);
        let q_cool =
            self.stages_cool as f64 * self.tons * 12_000.0 / self.cool.stages() as f64 * capacity;
        let q_heat =
            self.stages_heat as f64 * self.heat_mbh * 1000.0 * 0.8 / self.heat.stages() as f64;
        let sat_target = if self.fan_running {
            (self.mat + (q_heat - q_cool) / (1.08 * cfm)).clamp(40.0, 140.0)
        } else {
            self.mat
        };
        self.sat = lag(
            self.sat,
            sat_target,
            if self.fan_running { 75.0 } else { 600.0 },
            dt,
        );
        sense(db, m, p.sa_t, self.sat);
        sense(db, m, p.ma_t, self.mat);
        sense(db, m, p.ra_t, rat);

        let air = &mut bus.air[self.zone];
        air.supply(cfm, self.sat, oaf);
        air.setpoints = Some((csp, hsp));

        // filter loads over ~4 months of run time; the switch trips at 0.8 in.
        self.filter_load = (self.filter_load + dt * cfm / self.cfm / (120.0 * 86_400.0)).min(1.0);
        let dp = (0.3 + 0.6 * self.filter_load + m.filter_dp[self.dev]) * (cfm / self.cfm).powi(2);
        status(db, p.flt, dp > 0.8);

        let fan_kw = if self.fan_running {
            self.cfm * 0.0007
        } else {
            0.0
        };
        let comp_kw = self.stages_cool as f64 / self.cool.stages() as f64
            * self.tons
            * 1.1
            * (1.0 + 0.012 * (oat - 95.0)).max(0.75);
        let heat_kw = if self.heating == Heating::Electric {
            q_heat / 0.8 / 3412.0
        } else {
            0.0
        };
        if self.heating != Heating::Electric {
            bus.energy.gas_btuh += q_heat / 0.8;
        }
        self.kw = fan_kw + comp_kw + heat_kw;
        bus.energy.hvac_kw += self.kw;
        db.set_real(p.kw, self.kw);

        self.mode = if !self.fan_running {
            1
        } else if self.stages_heat > 0 {
            6
        } else if econ && clg > 0.0 && self.stages_cool > 0 {
            5
        } else if econ && clg > 0.0 {
            4
        } else if self.stages_cool > 0 {
            3
        } else {
            2
        };
        program_multi(db, p.mode, self.mode);
    }

    pub fn headline(&self) -> String {
        let what = MODES[self.mode as usize - 1];
        match self.mode {
            1 => "Off".into(),
            3 | 5 => format!(
                "{what} · {} of {} stages · SA {}",
                self.stages_cool,
                self.cool.stages(),
                fmt_f(self.sat)
            ),
            6 => format!(
                "Heating · {} of {} stages · SA {}",
                self.stages_heat,
                self.heat.stages(),
                fmt_f(self.sat)
            ),
            _ => format!("{what} · SA {}", fmt_f(self.sat)),
        }
    }
}
