//! VAV air handler: variable-speed supply fan on duct static pressure,
//! discharge air temperature from one split-range loop (heat → economizer →
//! cooling), night cycling when a zone falls out of its unoccupied band, and
//! a low-temperature detector that trips the fan on freezing mixed air.

use super::*;
use crate::control::{DeadTime, Delay, Pi, Stager, Stroke, lag};
use crate::package::{AhuDef, Cooling, Heating};
use crate::scenario::Actuator;
use crate::zone::Zone;

const MODES: [&str; 5] = ["Off", "Occupied", "Night Heat", "Night Cool", "Safety Trip"];
/// Duct static at full speed and no flow; flow pulls it down from there.
const SHUTOFF_INWC: f64 = 3.6;

/// What the boxes downstream see.
#[derive(Clone, Copy, Debug, Default)]
pub struct AhuOut {
    pub running: bool,
    pub sat: f64,
    pub static_inwc: f64,
    pub oa_fraction: f64,
    /// Night cycle: -1 heating, 1 cooling.
    pub night: i8,
}

struct Points {
    sf_c: PointRef,
    sf_s: PointRef,
    sf_spd: PointRef,
    dsp: PointRef,
    dsp_sp: PointRef,
    sa_t: PointRef,
    sa_t_sp: PointRef,
    ma_t: PointRef,
    ra_t: PointRef,
    oa_t: PointRef,
    ra_co2: PointRef,
    sa_flow: PointRef,
    oad: PointRef,
    min_oa: PointRef,
    clg_vlv: Option<PointRef>,
    clg_stages: Vec<PointRef>,
    htg_vlv: Option<PointRef>,
    htg_stages: Vec<PointRef>,
    flt_dp: PointRef,
    occ_mode: PointRef,
    occ: PointRef,
    econ: PointRef,
    mode: PointRef,
    kw: PointRef,
    sf_fail: PointRef,
    lt_det: PointRef,
    sat_loop: PointRef,
    sat_tuning: Tuning,
    dsp_tuning: Tuning,
}

pub struct Ahu {
    pub dev: usize,
    pub id: String,
    design_cfm: f64,
    cooling: Cooling,
    tons: f64,
    heating: Heating,
    heat_mbh: f64,
    economizer: bool,
    pub chw: Option<usize>,
    pub hw: Option<usize>,
    pub vavs: Vec<usize>,
    p: Points,
    static_pi: Pi,
    dat_pi: Pi,
    oad: Stroke,
    clg_vlv: Stroke,
    htg_vlv: Stroke,
    /// Coil metal and water, then the air on its way to the sensor.
    coil_t: f64,
    sat_delay: DeadTime,
    dsp_delay: DeadTime,
    cool: Stager,
    heat: Stager,
    sf_delay: Delay,
    lt_delay: Delay,
    lt_tripped_s: Option<f64>,
    night_hold_s: f64,
    speed: f64,
    flow: f64,
    mat: f64,
    rat: f64,
    filter_load: f64,
    pub out: AhuOut,
    pub occupied: bool,
    pub mode: u32,
    pub econ: bool,
    pub clg_out: f64,
    pub htg_out: f64,
    pub kw: f64,
}

impl Ahu {
    pub fn new(def: &AhuDef, chw: Option<usize>, hw: Option<usize>, b: &mut DevBuilder) -> Self {
        let p = Points {
            sf_c: b.bo("SF-C", "Supply fan command", ["Off", "On"]),
            sf_s: b.bi("SF-S", "Supply fan status", ["Off", "On"]),
            sf_spd: b.ao(
                "SF-SPD",
                "Supply fan VFD speed command",
                Units::Percent,
                0.0,
            ),
            dsp: b.ai("DSP", "Duct static pressure", Units::InchesOfWater, 0.0),
            dsp_sp: b.sp(
                "DSP-SP",
                "Duct static pressure setpoint",
                Units::InchesOfWater,
                1.25,
                0.25,
                2.5,
            ),
            sa_t: b.ai(
                "SA-T",
                "Supply air temperature",
                Units::DegreesFahrenheit,
                65.0,
            ),
            sa_t_sp: b.sp(
                "SA-T-SP",
                "Supply air temperature setpoint",
                Units::DegreesFahrenheit,
                55.0,
                48.0,
                70.0,
            ),
            ma_t: b.ai(
                "MA-T",
                "Mixed air temperature",
                Units::DegreesFahrenheit,
                65.0,
            ),
            ra_t: b.ai(
                "RA-T",
                "Return air temperature",
                Units::DegreesFahrenheit,
                72.0,
            ),
            oa_t: b.ai(
                "OA-T",
                "Outside air temperature (unit sensor)",
                Units::DegreesFahrenheit,
                60.0,
            ),
            ra_co2: b.ai("RA-CO2", "Return air CO2", Units::PartsPerMillion, 450.0),
            sa_flow: b.ai("SA-FLOW", "Supply airflow", Units::CubicFeetPerMinute, 0.0),
            oad: b.ao("OAD-O", "Outside air damper command", Units::Percent, 0.0),
            min_oa: b.sp(
                "MIN-OA",
                "Minimum outside air damper position",
                Units::Percent,
                def.min_oa_pct,
                0.0,
                100.0,
            ),
            clg_vlv: (def.cooling == Cooling::Chw).then(|| {
                b.ao(
                    "CLG-VLV",
                    "Chilled water valve command",
                    Units::Percent,
                    0.0,
                )
            }),
            clg_stages: if def.cooling == Cooling::Dx {
                (1..=2)
                    .map(|i| {
                        b.bo(
                            &format!("CLG{i}-C"),
                            &format!("DX cooling stage {i} command"),
                            ["Off", "On"],
                        )
                    })
                    .collect()
            } else {
                Vec::new()
            },
            htg_vlv: (def.heating == Heating::Hw)
                .then(|| b.ao("HTG-VLV", "Hot water valve command", Units::Percent, 0.0)),
            htg_stages: if matches!(def.heating, Heating::Gas | Heating::Electric) {
                (1..=2)
                    .map(|i| {
                        b.bo(
                            &format!("HTG{i}-C"),
                            &format!("Heating stage {i} command"),
                            ["Off", "On"],
                        )
                    })
                    .collect()
            } else {
                Vec::new()
            },
            flt_dp: b.ai(
                "FLT-DP",
                "Filter differential pressure",
                Units::InchesOfWater,
                0.0,
            ),
            occ_mode: b.occ_mode(),
            occ: b.occ_status(),
            econ: b.bv("ECON", "Economizer active", ["Off", "On"]),
            mode: b.msv("UNIT-MODE", "Unit mode", &MODES, 1, false),
            kw: b.av("KW", "Unit power", Units::Kilowatts, 0.0),
            sf_fail: b.alarm("SF-FAIL", "Supply fan failure: commanded on, no status"),
            lt_det: b.alarm("LT-DET", "Low temperature detection (freezestat) trip"),
            sat_loop: b.av(
                "SAT-LOOP",
                "Supply air loop output: below 0 heats, 0–50 economizer, 50–100 cools",
                Units::Percent,
                0.0,
            ),
            sat_tuning: b.tuning("SAT", "Supply air temperature", "°F", 6.0, 120.0, 200.0),
            dsp_tuning: b.tuning("DSP", "Duct static pressure", "in. wc", 15.0, 20.0, 500.0),
        };
        Ahu {
            dev: b.device(),
            id: def.common.id.clone(),
            design_cfm: def.supply_cfm,
            cooling: def.cooling,
            tons: def.cool_tons,
            heating: def.heating,
            heat_mbh: def.heat_mbh,
            economizer: def.economizer,
            chw,
            hw,
            vavs: Vec::new(),
            p,
            static_pi: Pi::new(15.0, 20.0, 20.0, 100.0),
            dat_pi: Pi::new(6.0, 120.0, -100.0, 100.0),
            oad: Stroke::new(90.0),
            clg_vlv: Stroke::new(60.0),
            htg_vlv: Stroke::new(60.0),
            coil_t: 65.0,
            sat_delay: DeadTime::new(8.0),
            dsp_delay: DeadTime::new(1.5),
            cool: Stager::new(2, 180.0, 300.0),
            heat: Stager::new(2, 120.0, 180.0),
            sf_delay: Delay::default(),
            lt_delay: Delay::default(),
            lt_tripped_s: None,
            night_hold_s: 0.0,
            speed: 0.0,
            flow: 0.0,
            mat: 65.0,
            rat: 72.0,
            filter_load: 0.2,
            out: AhuOut {
                sat: 65.0,
                ..Default::default()
            },
            occupied: false,
            mode: 1,
            econ: false,
            clg_out: 0.0,
            htg_out: 0.0,
            kw: 0.0,
        }
    }

    pub fn step(
        &mut self,
        env: &Env,
        db: &mut PointDb,
        zones: &[Zone],
        vavs: &[Vav],
        bus: &mut Bus,
    ) {
        let (dt, m) = (env.dt, env.mods);
        let p = &self.p;
        let oat = env.weather.oat;
        let dead = m.shutdown();
        sense(db, m, p.oa_t, oat);

        // return air: flow-weighted zones, a degree of plenum heat on top
        let (mut f, mut ft, mut fc, mut t, mut c) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for &v in &self.vavs {
            let z = &zones[vavs[v].zone];
            f += vavs[v].flow;
            ft += vavs[v].flow * z.temp;
            fc += vavs[v].flow * z.co2;
            t += z.temp;
            c += z.co2;
        }
        let n = self.vavs.len().max(1) as f64;
        let (zone_t, zone_co2) = if f > 50.0 {
            (ft / f, fc / f)
        } else {
            (t / n, c / n)
        };
        self.rat = lag(self.rat, zone_t + 1.0, 60.0, dt);

        // occupancy, or night cycle when a box calls
        self.occupied = occupied(db.read_u32(p.occ_mode), env.occupied);
        db.set_bool(p.occ, self.occupied);
        let call = if self.occupied {
            0
        } else {
            self.vavs
                .iter()
                .map(|&v| vavs[v].night_call)
                .find(|c| *c != 0)
                .unwrap_or(0)
        };
        if call != 0 {
            self.night_hold_s = 600.0;
            self.out.night = call;
        } else {
            self.night_hold_s = (self.night_hold_s - dt).max(0.0);
            if self.night_hold_s <= 0.0 {
                self.out.night = 0;
            }
        }
        if self.occupied {
            self.out.night = 0;
        }
        let wants_run = self.occupied || self.out.night != 0;

        // safeties: low-temperature trip latches for five minutes
        if let Some(t) = self.lt_tripped_s.as_mut() {
            *t += dt;
            if *t > 300.0 && self.mat > 45.0 {
                self.lt_tripped_s = None;
            }
        }
        let fan_cmd = program_bool(db, p.sf_c, wants_run);
        let running = fan_cmd && !m.fail_fan[self.dev] && !dead && self.lt_tripped_s.is_none();
        status(db, p.sf_s, running);
        let a = self.sf_delay.update(
            fan_cmd && !running && !dead && self.lt_tripped_s.is_none(),
            30.0,
            dt,
        );
        alarm(db, p.sf_fail, a);

        // fan speed on duct static
        let static_sp = db.read_f64(p.dsp_sp);
        let spd = if running {
            p.dsp_tuning.apply(db, &mut self.static_pi);
            self.static_pi.update(static_sp - db.read_f64(p.dsp), dt)
        } else {
            self.static_pi.preset(30.0);
            0.0
        };
        let spd_cmd = program_real(db, p.sf_spd, spd);
        self.speed = lag(self.speed, if running { spd_cmd } else { 0.0 }, 8.0, dt);

        // discharge air: heat <- 0 -> economizer -> cooling
        let night_heat = self.out.night < 0 && !self.occupied;
        let sat_sp = if night_heat {
            90.0
        } else {
            db.read_f64(p.sa_t_sp)
        };
        let sat_reading = db.read_f64(p.sa_t);
        p.sat_tuning.apply(db, &mut self.dat_pi);
        let u = if running {
            self.dat_pi.update(sat_reading - sat_sp, dt)
        } else {
            self.dat_pi.reset();
            0.0
        };
        let base_oa = if self.occupied {
            db.read_f64(p.min_oa)
        } else {
            0.0
        };
        self.econ = running
            && self.economizer
            && !night_heat
            && oat < 65.0
            && oat < self.rat - 2.0
            && u > 0.0;
        let (mut oad, mut clg, htg) = (base_oa, 0.0, (-u).max(0.0));
        if u > 0.0 {
            if self.econ {
                oad = base_oa + (100.0 - base_oa) * (u / 50.0).min(1.0);
                clg = ((u - 50.0) * 2.0).max(0.0);
            } else {
                clg = u;
            }
        }
        // mixed-air low limit
        if self.mat < 45.0 {
            oad = base_oa;
        }
        if !running {
            oad = 0.0;
        }
        let oad_cmd = program_real(db, p.oad, oad);
        db.set_real(p.sat_loop, u);
        db.set_bool(p.econ, self.econ);

        let (clg_cmd, htg_cmd) = (
            self.cooling_cmd(db, clg, running && htg <= 0.0 && oat > 40.0, dt),
            self.heating_cmd(db, htg, running && clg <= 0.0, dt),
        );
        self.clg_out = clg_cmd;
        self.htg_out = htg_cmd;

        // airflow and duct static from the fan curve and what the boxes take
        let p = &self.p;
        self.flow = if running {
            self.vavs.iter().map(|&v| vavs[v].flow).sum::<f64>()
        } else {
            0.0
        };
        let s = self.speed / 100.0;
        let k = (SHUTOFF_INWC - 1.6) / self.design_cfm.powi(2);
        let static_target = if running {
            (SHUTOFF_INWC * s * s - k * self.flow * self.flow).max(0.0)
        } else {
            0.0
        };
        self.out.static_inwc = lag(self.out.static_inwc, static_target, 3.0, dt);

        // spring return: the damper closes when the fan stops
        let oad_pos = self.oad.update(if running { oad_cmd } else { 0.0 }, dt);
        let oad_actual = m.stuck(self.dev, Actuator::OaDamper).unwrap_or(oad_pos);
        let oaf = (oad_actual / 100.0).clamp(0.0, 1.0);
        let mat_target = if running {
            oaf * oat + (1.0 - oaf) * self.rat
        } else {
            0.85 * self.rat + 0.15 * oat
        };
        self.mat = lag(self.mat, mat_target, if running { 20.0 } else { 600.0 }, dt);
        if running && self.lt_delay.update(self.mat < 38.0, 60.0, dt) {
            self.lt_tripped_s = Some(0.0);
        }
        alarm(db, p.lt_det, self.lt_tripped_s.is_some());

        // coils
        let cfm = self.flow.max(1.0);
        let mut after_cool = self.mat;
        let mut chw_load = 0.0;
        let cool_ok = !m.fail_cool[self.dev];
        let clg_pos = self.clg_vlv.update(clg_cmd, dt);
        let clg_actual = m.stuck(self.dev, Actuator::CoolingValve).unwrap_or(clg_pos);
        match self.cooling {
            Cooling::Chw => {
                let plant = self.chw.map(|i| bus.chw[i]).unwrap_or(PlantOut {
                    supply_t: 44.0,
                    flowing: true,
                });
                if running && plant.flowing && cool_ok {
                    let eff = 0.85 * clg_actual / 100.0;
                    after_cool = self.mat - eff * (self.mat - (plant.supply_t + 3.0)).max(0.0);
                    chw_load = 1.08 * self.flow * (self.mat - after_cool) * 1.25;
                }
                if let Some(i) = self.chw {
                    bus.chw_demand[i].add(chw_load, if running { clg_actual } else { 0.0 });
                }
            }
            Cooling::Dx => {
                let stages = self.cool.count() as f64;
                if running && cool_ok && stages > 0.0 {
                    let capacity = (1.0 - 0.006 * (oat - 95.0)).clamp(0.7, 1.15);
                    after_cool = (self.mat
                        - stages / 2.0 * self.tons * 12_000.0 * capacity / (1.08 * cfm))
                        .max(42.0);
                }
            }
            Cooling::None => {}
        }
        let heat_ok = !m.fail_heat[self.dev];
        let htg_pos = self.htg_vlv.update(htg_cmd, dt);
        let htg_actual = m.stuck(self.dev, Actuator::HeatingValve).unwrap_or(htg_pos);
        let mut rise = 0.0;
        match self.heating {
            Heating::Hw => {
                let plant = self.hw.map(|i| bus.hw[i]).unwrap_or(PlantOut {
                    supply_t: 160.0,
                    flowing: true,
                });
                if running && plant.flowing && heat_ok {
                    rise = 0.6 * htg_actual / 100.0 * (plant.supply_t - after_cool).max(0.0);
                }
                if let Some(i) = self.hw {
                    bus.hw_demand[i].add(
                        1.08 * self.flow * rise,
                        if running { htg_actual } else { 0.0 },
                    );
                }
            }
            Heating::Gas | Heating::Electric => {
                if running && heat_ok {
                    rise = self.heat.count() as f64 / 2.0 * self.heat_mbh * 800.0 / (1.08 * cfm);
                }
            }
            Heating::None => {}
        }
        let sat_target = if running {
            (after_cool + rise).clamp(38.0, 140.0)
        } else {
            self.mat
        };
        self.coil_t = lag(
            self.coil_t,
            sat_target,
            if running { 30.0 } else { 600.0 },
            dt,
        );
        self.out.sat = lag(
            self.out.sat,
            self.coil_t,
            if running { 10.0 } else { 600.0 },
            dt,
        );
        let sat_reading = self.sat_delay.update(self.out.sat, dt);
        self.out.running = running;
        self.out.oa_fraction = if running { oaf } else { 0.0 };

        sense_noisy(db, m, p.sa_t, sat_reading, 0.04, env.now);
        sense(db, m, p.ma_t, self.mat);
        sense(db, m, p.ra_t, self.rat);
        sense(db, m, p.ra_co2, zone_co2);
        let dsp_reading = self.dsp_delay.update(self.out.static_inwc, dt);
        sense_noisy(db, m, p.dsp, dsp_reading, 0.015, env.now);
        sense_noisy(db, m, p.sa_flow, self.flow, self.flow * 0.015, env.now);
        self.filter_load =
            (self.filter_load + dt * self.flow / self.design_cfm / (120.0 * 86_400.0)).min(1.0);
        let dp = (0.35 + 0.5 * self.filter_load + m.filter_dp[self.dev])
            * (self.flow / self.design_cfm).powi(2);
        sense(db, m, p.flt_dp, dp);

        let fan_kw = self.design_cfm * 0.0009 * s.powi(3);
        let dx_kw = if self.cooling == Cooling::Dx && running && cool_ok {
            self.cool.count() as f64 / 2.0 * self.tons * 1.1
        } else {
            0.0
        };
        let heat_btuh = if matches!(self.heating, Heating::Gas | Heating::Electric) {
            1.08 * self.flow * rise
        } else {
            0.0
        };
        match self.heating {
            Heating::Gas => bus.energy.gas_btuh += heat_btuh / 0.8,
            Heating::Electric => bus.energy.hvac_kw += heat_btuh / 3412.0,
            _ => {}
        }
        self.kw = fan_kw + dx_kw;
        bus.energy.hvac_kw += self.kw;
        db.set_real(p.kw, self.kw);

        self.mode = if self.lt_tripped_s.is_some() {
            5
        } else if !running {
            1
        } else if self.occupied {
            2
        } else if self.out.night < 0 {
            3
        } else {
            4
        };
        program_multi(db, p.mode, self.mode);
    }

    /// Cooling output: a valve % or DX stages; returns the valve or stage %.
    fn cooling_cmd(&mut self, db: &mut PointDb, demand: f64, enabled: bool, dt: f64) -> f64 {
        if let Some(v) = self.p.clg_vlv {
            return program_real(db, v, if enabled { demand } else { 0.0 });
        }
        self.cool.update(demand, enabled, dt);
        let on: Vec<bool> = self
            .p
            .clg_stages
            .clone()
            .iter()
            .enumerate()
            .map(|(i, r)| program_bool(db, *r, self.cool.on(i)))
            .collect();
        on.iter().filter(|o| **o).count() as f64 / on.len().max(1) as f64 * 100.0
    }

    fn heating_cmd(&mut self, db: &mut PointDb, demand: f64, enabled: bool, dt: f64) -> f64 {
        if let Some(v) = self.p.htg_vlv {
            return program_real(db, v, if enabled { demand } else { 0.0 });
        }
        self.heat.update(demand, enabled, dt);
        let on: Vec<bool> = self
            .p
            .htg_stages
            .clone()
            .iter()
            .enumerate()
            .map(|(i, r)| program_bool(db, *r, self.heat.on(i)))
            .collect();
        on.iter().filter(|o| **o).count() as f64 / on.len().max(1) as f64 * 100.0
    }

    pub fn headline(&self) -> String {
        match self.mode {
            1 => "Off".into(),
            5 => "Tripped on low temperature".into(),
            _ => {
                let mut s = format!(
                    "{} · fan {:.0} % · {:.2} in. · SA {}",
                    MODES[self.mode as usize - 1],
                    self.speed,
                    self.out.static_inwc,
                    fmt_f(self.out.sat)
                );
                if self.econ {
                    s.push_str(" · econ");
                }
                s
            }
        }
    }
}
