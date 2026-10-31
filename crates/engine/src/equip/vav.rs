//! Pressure-independent VAV box with reheat: the cooling loop resets the
//! airflow setpoint between minimum and maximum, the heating loop holds
//! minimum flow and opens the reheat. An airflow loop drives a floating
//! damper actuator against whatever duct static the air handler is making,
//! so a hot-headed flow loop hunts the way a real one does.

use super::*;
use crate::control::{DeadTime, Pi, Stroke, lag};
use crate::package::{Heating, VavDef};
use crate::scenario::Actuator;
use crate::zone::Zone;

struct Points {
    zn_t: PointRef,
    zn_co2: Option<PointRef>,
    da_t: PointRef,
    flow: PointRef,
    flow_sp: PointRef,
    dmpr: PointRef,
    rht: Option<PointRef>,
    occ_clg: PointRef,
    occ_htg: PointRef,
    unocc_clg: PointRef,
    unocc_htg: PointRef,
    max_flow: PointRef,
    min_flow: PointRef,
    eff_clg: PointRef,
    eff_htg: PointRef,
    clg_loop: PointRef,
    htg_loop: PointRef,
    occ_mode: PointRef,
    occ: PointRef,
    clg_tuning: Tuning,
    htg_tuning: Tuning,
    flow_tuning: Tuning,
}

pub struct Vav {
    pub dev: usize,
    pub id: String,
    pub ahu: usize,
    pub zone: usize,
    pub hw: Option<usize>,
    reheat: Heating,
    reheat_kw: f64,
    max_cfm: f64,
    p: Points,
    clg_pi: Pi,
    htg_pi: Pi,
    flow_pi: Pi,
    damper: Stroke,
    rht_vlv: Stroke,
    flow_reading: f64,
    flow_delay: DeadTime,
    /// A wall sensor trails the room by a minute or two.
    zone_reading: Option<f64>,
    dat: f64,
    pub flow: f64,
    pub flow_sp: f64,
    /// While unoccupied: -1 wants heat, 1 wants cooling.
    pub night_call: i8,
    pub occupied: bool,
    pub reheat_pct: f64,
}

impl Vav {
    pub fn new(
        def: &VavDef,
        ahu: usize,
        zone: usize,
        hw: Option<usize>,
        b: &mut DevBuilder,
    ) -> Self {
        let min = def.min_cfm.unwrap_or((def.max_cfm * 0.3).round());
        let p = Points {
            zn_t: b.ai("ZN-T", "Zone temperature", Units::DegreesFahrenheit, 72.0),
            zn_co2: def
                .co2
                .then(|| b.ai("ZN-CO2", "Zone CO2", Units::PartsPerMillion, 450.0)),
            da_t: b.ai(
                "DA-T",
                "Discharge air temperature",
                Units::DegreesFahrenheit,
                65.0,
            ),
            flow: b.ai("AIRFLOW", "Airflow", Units::CubicFeetPerMinute, 0.0),
            flow_sp: b.av(
                "AIRFLOW-SP",
                "Airflow setpoint",
                Units::CubicFeetPerMinute,
                0.0,
            ),
            dmpr: b.ao("DMPR-O", "Damper command", Units::Percent, 0.0),
            rht: match def.reheat {
                Heating::Hw => Some(b.ao("RHT-VLV", "Reheat valve command", Units::Percent, 0.0)),
                Heating::Electric | Heating::Gas => {
                    Some(b.ao("RHT-O", "Electric reheat output (SCR)", Units::Percent, 0.0))
                }
                Heating::None => None,
            },
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
                82.0,
                60.0,
                95.0,
            ),
            unocc_htg: b.sp(
                "UNOCC-HTG-SP",
                "Unoccupied heating setpoint",
                Units::DegreesFahrenheit,
                62.0,
                40.0,
                80.0,
            ),
            max_flow: b.sp(
                "MAX-FLOW",
                "Maximum cooling airflow",
                Units::CubicFeetPerMinute,
                def.max_cfm,
                0.0,
                def.max_cfm * 1.5,
            ),
            min_flow: b.sp(
                "MIN-FLOW",
                "Minimum airflow",
                Units::CubicFeetPerMinute,
                min,
                0.0,
                def.max_cfm,
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
            clg_loop: b.av("CLG-LOOP", "Cooling loop output", Units::Percent, 0.0),
            htg_loop: b.av("HTG-LOOP", "Heating loop output", Units::Percent, 0.0),
            occ_mode: b.occ_mode(),
            occ: b.occ_status(),
            clg_tuning: b.tuning("CLG", "Zone cooling", "°F", 40.0, 1200.0, 500.0),
            htg_tuning: b.tuning("HTG", "Zone heating", "°F", 40.0, 1200.0, 500.0),
            flow_tuning: b.tuning("FLOW", "Airflow", "% of max flow", 0.6, 45.0, 50.0),
        };
        Vav {
            dev: b.device(),
            id: def.common.id.clone(),
            ahu,
            zone,
            hw,
            reheat: def.reheat,
            reheat_kw: def.reheat_kw,
            max_cfm: def.max_cfm,
            p,
            clg_pi: Pi::new(40.0, 1200.0, 0.0, 100.0),
            htg_pi: Pi::new(40.0, 1200.0, 0.0, 100.0),
            flow_pi: Pi::new(0.6, 45.0, 0.0, 100.0),
            // a typical floating actuator: 90 s end to end
            damper: Stroke::new(90.0),
            rht_vlv: Stroke::new(60.0),
            flow_reading: 0.0,
            flow_delay: DeadTime::new(2.0),
            zone_reading: None,
            dat: 65.0,
            flow: 0.0,
            flow_sp: 0.0,
            night_call: 0,
            occupied: false,
            reheat_pct: 0.0,
        }
    }

    pub fn step(
        &mut self,
        env: &Env,
        db: &mut PointDb,
        zones: &[Zone],
        ahu: &AhuOut,
        bus: &mut Bus,
    ) {
        let (dt, m, p) = (env.dt, env.mods, &self.p);
        let zone = &zones[self.zone];
        let zr = lag(self.zone_reading.unwrap_or(zone.temp), zone.temp, 90.0, dt);
        self.zone_reading = Some(zr);
        sense(db, m, p.zn_t, zr);
        if let Some(r) = p.zn_co2 {
            sense(db, m, r, zone.co2);
        }

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
        let htg = self.htg_pi.update(hsp - zt, dt);
        db.set_real(p.clg_loop, clg);
        db.set_real(p.htg_loop, htg);

        // night calls, with a couple of degrees of hysteresis
        self.night_call = if self.occupied {
            0
        } else if zt < hsp || (self.night_call < 0 && zt < hsp + 2.0) {
            -1
        } else if zt > csp || (self.night_call > 0 && zt > csp - 2.0) {
            1
        } else {
            0
        };

        let (max, min) = (db.read_f64(p.max_flow), db.read_f64(p.min_flow));
        self.flow_sp = if self.occupied {
            min + clg / 100.0 * (max - min).max(0.0)
        } else if ahu.night > 0 && clg > 0.0 {
            (max * clg / 100.0).max(min)
        } else if ahu.night < 0 && htg > 0.0 {
            min.max(max * 0.4)
        } else {
            0.0
        };
        db.set_real(p.flow_sp, self.flow_sp);

        // airflow loop on the (noisy) flow reading, in % of max flow; with
        // no static to work against it drives the damper wide open
        p.flow_tuning.apply(db, &mut self.flow_pi);
        let want = if self.flow_sp <= 0.0 {
            self.flow_pi.reset();
            0.0
        } else {
            let error = (self.flow_sp - db.read_f64(p.flow)) / self.max_cfm * 100.0;
            self.flow_pi.update(error, dt)
        };
        let dmpr_cmd = program_real(db, p.dmpr, want);
        let position = self.damper.update(dmpr_cmd, dt);
        let position = m.stuck(self.dev, Actuator::Damper).unwrap_or(position);
        let avail = if ahu.running {
            self.max_cfm * 1.3 * ahu.static_inwc.max(0.0).sqrt()
        } else {
            0.0
        };
        self.flow = lag(self.flow, avail * position / 100.0, 4.0, dt);
        self.flow_reading = lag(
            self.flow_reading,
            self.flow_delay.update(self.flow, dt),
            2.0,
            dt,
        );
        sense_noisy(
            db,
            m,
            p.flow,
            self.flow_reading,
            self.flow_reading * 0.015,
            env.now,
        );

        // reheat needs air across it
        let heat_ok = !m.fail_heat[self.dev] && self.flow > 0.25 * min.max(50.0);
        let rht_cmd = match p.rht {
            Some(r) => program_real(
                db,
                r,
                if heat_ok && (self.occupied || ahu.night < 0) {
                    htg
                } else {
                    0.0
                },
            ),
            None => 0.0,
        };
        let rht_pos = if self.reheat == Heating::Hw {
            self.rht_vlv.update(rht_cmd, dt)
        } else {
            rht_cmd
        };
        self.reheat_pct = m.stuck(self.dev, Actuator::HeatingValve).unwrap_or(rht_pos);
        let rise = match self.reheat {
            _ if !heat_ok => 0.0,
            Heating::Hw => {
                let plant = self.hw.map(|i| bus.hw[i]).unwrap_or(PlantOut {
                    supply_t: 160.0,
                    flowing: true,
                });
                let r = if plant.flowing {
                    0.55 * self.reheat_pct / 100.0 * (plant.supply_t - ahu.sat).max(0.0)
                } else {
                    0.0
                };
                if let Some(i) = self.hw {
                    bus.hw_demand[i].add(1.08 * self.flow * r, self.reheat_pct);
                }
                r
            }
            Heating::Electric | Heating::Gas => {
                let kw = self.reheat_pct / 100.0 * self.reheat_kw;
                bus.energy.hvac_kw += kw;
                (kw * 3412.0 / (1.08 * self.flow.max(50.0))).min(45.0)
            }
            Heating::None => 0.0,
        };
        let dat_target = if self.flow > 10.0 {
            ahu.sat + rise
        } else {
            zone.temp
        };
        self.dat = lag(self.dat, dat_target, 30.0, dt);
        sense(db, m, p.da_t, self.dat);

        let air = &mut bus.air[self.zone];
        air.supply(self.flow, self.dat, ahu.oa_fraction);
        air.setpoints = Some((csp, hsp));
    }

    pub fn headline(&self) -> String {
        let mut s = format!(
            "{:.0} of {:.0} cfm · DA {}",
            self.flow,
            self.flow_sp,
            fmt_f(self.dat)
        );
        if self.reheat_pct > 1.0 {
            s.push_str(&format!(" · reheat {:.0} %", self.reheat_pct));
        }
        s
    }
}
