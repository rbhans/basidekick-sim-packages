//! Central plants: a chiller with its chilled-water pump, a boiler with its
//! hot-water pump. Each loop is one lump of water: coils add or take heat,
//! the machine works to hold the supply temperature.

use super::*;
use crate::control::{Delay, lag};
use crate::package::{BoilerDef, ChillerDef};

/// A loop's supply, as the coils see it.
#[derive(Clone, Copy, Debug)]
pub struct PlantOut {
    pub supply_t: f64,
    pub flowing: bool,
}

impl Default for PlantOut {
    fn default() -> Self {
        PlantOut {
            supply_t: 70.0,
            flowing: false,
        }
    }
}

/// What the coils on a loop asked of it in one step.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlantDemand {
    /// Heat moved through the coils, BTU/h (positive both ways).
    pub load_btuh: f64,
    /// Widest valve on the loop, %.
    pub max_valve: f64,
}

impl PlantDemand {
    pub fn add(&mut self, load_btuh: f64, valve: f64) {
        self.load_btuh += load_btuh.max(0.0);
        self.max_valve = self.max_valve.max(valve);
    }
}

/// Plant enable with a minimum run so it doesn't short-cycle on a
/// borderline call.
#[derive(Clone, Debug, Default)]
struct EnableHold {
    left_s: f64,
}

impl EnableHold {
    fn update(&mut self, call: bool, dt: f64) -> bool {
        self.left_s = if call {
            900.0
        } else {
            (self.left_s - dt).max(0.0)
        };
        self.left_s > 0.0
    }
}

struct ChwPoints {
    enable: PointRef,
    ch_c: PointRef,
    ch_s: PointRef,
    pump_c: PointRef,
    pump_s: PointRef,
    chws: PointRef,
    chwr: PointRef,
    chws_sp: PointRef,
    flow: PointRef,
    load: PointRef,
    kw: PointRef,
    ch_alm: PointRef,
    pump_fail: PointRef,
}

pub struct ChillerPlant {
    pub dev: usize,
    pub id: String,
    tons: f64,
    gpm: f64,
    p: ChwPoints,
    hold: EnableHold,
    proven: Delay,
    alm_delay: Delay,
    pump_delay: Delay,
    supply_t: f64,
    return_t: f64,
    pub out: PlantOut,
    pub enabled: bool,
    pub running: bool,
    pub load_pct: f64,
}

impl ChillerPlant {
    pub fn new(def: &ChillerDef, b: &mut DevBuilder) -> Self {
        let p = ChwPoints {
            enable: b.bv_cmd(
                "PLANT-EN",
                "Chilled water plant enable",
                ["Disabled", "Enabled"],
                false,
            ),
            ch_c: b.bo("CH-C", "Chiller command", ["Off", "On"]),
            ch_s: b.bi("CH-S", "Chiller status", ["Off", "On"]),
            pump_c: b.bo("CHWP-C", "Chilled water pump command", ["Off", "On"]),
            pump_s: b.bi("CHWP-S", "Chilled water pump status", ["Off", "On"]),
            chws: b.ai(
                "CHWS-T",
                "Chilled water supply temperature",
                Units::DegreesFahrenheit,
                55.0,
            ),
            chwr: b.ai(
                "CHWR-T",
                "Chilled water return temperature",
                Units::DegreesFahrenheit,
                56.0,
            ),
            chws_sp: b.sp(
                "CHWS-SP",
                "Chilled water supply setpoint",
                Units::DegreesFahrenheit,
                44.0,
                38.0,
                60.0,
            ),
            flow: b.ai(
                "CHW-FLOW",
                "Chilled water flow",
                Units::UsGallonsPerMinute,
                0.0,
            ),
            load: b.ai("CH-LOAD", "Chiller load", Units::Percent, 0.0),
            kw: b.ai("CH-KW", "Chiller plant power", Units::Kilowatts, 0.0),
            ch_alm: b.alarm("CH-ALM", "Chiller alarm (tripped)"),
            pump_fail: b.alarm("CHWP-FAIL", "Chilled water pump failure"),
        };
        ChillerPlant {
            dev: b.device(),
            id: def.common.id.clone(),
            tons: def.tons,
            gpm: def.tons * 2.4,
            p,
            hold: EnableHold::default(),
            proven: Delay::default(),
            alm_delay: Delay::default(),
            pump_delay: Delay::default(),
            supply_t: 58.0,
            return_t: 58.0,
            out: PlantOut {
                supply_t: 58.0,
                flowing: false,
            },
            enabled: false,
            running: false,
            load_pct: 0.0,
        }
    }

    /// `demand` is what the coils drew last step.
    pub fn step(&mut self, env: &Env, db: &mut PointDb, demand: PlantDemand, bus: &mut Bus) {
        let (dt, m, p) = (env.dt, env.mods, &self.p);
        let dead = m.shutdown();
        let call = demand.max_valve > 5.0 || (env.weather.oat > 62.0 && env.occupied);
        let auto = self.hold.update(call, dt);
        self.enabled = program_bool(db, p.enable, auto);

        let pump_cmd = program_bool(db, p.pump_c, self.enabled);
        let pump = pump_cmd && !m.fail_fan[self.dev] && !dead;
        status(db, p.pump_s, pump);
        let a = self.pump_delay.update(pump_cmd && !pump && !dead, 30.0, dt);
        alarm(db, p.pump_fail, a);
        let proven = self.proven.update(pump, 30.0, dt);

        let ch_cmd = program_bool(db, p.ch_c, self.enabled && proven);
        self.running = ch_cmd && pump && !m.fail_cool[self.dev] && !dead;
        status(db, p.ch_s, self.running);
        let a = self
            .alm_delay
            .update(ch_cmd && m.fail_cool[self.dev] && !dead, 20.0, dt);
        alarm(db, p.ch_alm, a);

        // the loop: coils warm it, the chiller pulls it back to setpoint
        let sp = db.read_f64(p.chws_sp);
        let capacity = self.tons * 12_000.0;
        let load = if pump { demand.load_btuh } else { 0.0 };
        let removed = if self.running {
            (load + capacity / 3.0 * (self.supply_t - sp)).clamp(0.0, capacity)
        } else {
            0.0
        };
        let mass = capacity * 0.04;
        // piping picks up a little heat from the building
        let gain = capacity * 0.0004 * (72.0 - self.supply_t);
        self.supply_t += (load + gain - removed) / mass * dt / 3600.0;
        let rise = if pump { load / (500.0 * self.gpm) } else { 0.0 };
        self.return_t = lag(
            self.return_t,
            self.supply_t + rise,
            if pump { 60.0 } else { 1800.0 },
            dt,
        );
        self.load_pct = removed / capacity * 100.0;
        self.out = PlantOut {
            supply_t: self.supply_t,
            flowing: pump,
        };

        sense(db, m, p.chws, self.supply_t);
        sense(db, m, p.chwr, self.return_t);
        sense(db, m, p.flow, if pump { self.gpm } else { 0.0 });
        sense(db, m, p.load, self.load_pct);
        let kw = removed / 12_000.0 * 0.62 + if pump { self.gpm * 0.015 } else { 0.0 };
        sense(db, m, p.kw, kw);
        bus.energy.hvac_kw += kw;
    }

    pub fn headline(&self) -> String {
        match (self.running, self.out.flowing) {
            (true, _) => format!(
                "Running · {:.0} % load · CHWS {}",
                self.load_pct,
                fmt_f(self.supply_t)
            ),
            (false, true) => format!("Pump only · CHWS {}", fmt_f(self.supply_t)),
            _ => format!("Off · CHWS {}", fmt_f(self.supply_t)),
        }
    }
}

struct HwPoints {
    enable: PointRef,
    b_c: PointRef,
    b_s: PointRef,
    pump_c: PointRef,
    pump_s: PointRef,
    hws: PointRef,
    hwr: PointRef,
    hws_sp: PointRef,
    fire: PointRef,
    b_alm: PointRef,
    pump_fail: PointRef,
}

pub struct BoilerPlant {
    pub dev: usize,
    pub id: String,
    mbh: f64,
    gpm: f64,
    p: HwPoints,
    hold: EnableHold,
    proven: Delay,
    alm_delay: Delay,
    pump_delay: Delay,
    supply_t: f64,
    return_t: f64,
    pub out: PlantOut,
    pub enabled: bool,
    pub running: bool,
    pub firing_pct: f64,
}

impl BoilerPlant {
    pub fn new(def: &BoilerDef, b: &mut DevBuilder) -> Self {
        let p = HwPoints {
            enable: b.bv_cmd(
                "PLANT-EN",
                "Hot water plant enable",
                ["Disabled", "Enabled"],
                false,
            ),
            b_c: b.bo("B-C", "Boiler command", ["Off", "On"]),
            b_s: b.bi("B-S", "Boiler status (flame proven)", ["Off", "On"]),
            pump_c: b.bo("HWP-C", "Hot water pump command", ["Off", "On"]),
            pump_s: b.bi("HWP-S", "Hot water pump status", ["Off", "On"]),
            hws: b.ai(
                "HWS-T",
                "Hot water supply temperature",
                Units::DegreesFahrenheit,
                90.0,
            ),
            hwr: b.ai(
                "HWR-T",
                "Hot water return temperature",
                Units::DegreesFahrenheit,
                88.0,
            ),
            hws_sp: b.sp(
                "HWS-SP",
                "Hot water supply setpoint (outdoor reset)",
                Units::DegreesFahrenheit,
                160.0,
                100.0,
                200.0,
            ),
            fire: b.ai("B-FIRE", "Boiler firing rate", Units::Percent, 0.0),
            b_alm: b.alarm("B-ALM", "Boiler alarm (flame failure lockout)"),
            pump_fail: b.alarm("HWP-FAIL", "Hot water pump failure"),
        };
        BoilerPlant {
            dev: b.device(),
            id: def.common.id.clone(),
            mbh: def.mbh,
            gpm: def.mbh / 10.0,
            p,
            hold: EnableHold::default(),
            proven: Delay::default(),
            alm_delay: Delay::default(),
            pump_delay: Delay::default(),
            supply_t: 90.0,
            return_t: 88.0,
            out: PlantOut {
                supply_t: 90.0,
                flowing: false,
            },
            enabled: false,
            running: false,
            firing_pct: 0.0,
        }
    }

    pub fn step(&mut self, env: &Env, db: &mut PointDb, demand: PlantDemand, bus: &mut Bus) {
        let (dt, m, p) = (env.dt, env.mods, &self.p);
        let dead = m.shutdown();
        let oat = env.weather.oat;
        let call = demand.max_valve > 5.0 || oat < 55.0;
        let auto = self.hold.update(call, dt);
        self.enabled = program_bool(db, p.enable, auto);
        // outdoor reset: 180 °F at 10 °F outside, 140 °F at 60 °F
        let reset = 180.0 - (oat.clamp(10.0, 60.0) - 10.0) * 0.8;
        db.set_program(p.hws_sp, PointValue::Real(reset as f32));
        let sp = db.read_f64(p.hws_sp);

        let pump_cmd = program_bool(db, p.pump_c, self.enabled);
        let pump = pump_cmd && !m.fail_fan[self.dev] && !dead;
        status(db, p.pump_s, pump);
        let a = self.pump_delay.update(pump_cmd && !pump && !dead, 30.0, dt);
        alarm(db, p.pump_fail, a);
        let proven = self.proven.update(pump, 20.0, dt);

        let b_cmd = program_bool(db, p.b_c, self.enabled && proven);
        self.running = b_cmd && pump && !m.fail_heat[self.dev] && !dead;
        status(db, p.b_s, self.running);
        let a = self
            .alm_delay
            .update(b_cmd && m.fail_heat[self.dev] && !dead, 20.0, dt);
        alarm(db, p.b_alm, a);

        let capacity = self.mbh * 1000.0;
        let load = if pump { demand.load_btuh } else { 0.0 };
        let added = if self.running {
            (load
                + capacity * 0.0004 * (self.supply_t - 70.0)
                + capacity / 3.0 * (sp - self.supply_t))
                .clamp(0.0, capacity)
        } else {
            0.0
        };
        let mass = capacity * 0.015;
        // piping loses a little heat to the building
        let loss = capacity * 0.0004 * (self.supply_t - 70.0);
        self.supply_t += (added - load - loss) / mass * dt / 3600.0;
        let drop = if pump { load / (500.0 * self.gpm) } else { 0.0 };
        self.return_t = lag(
            self.return_t,
            self.supply_t - drop,
            if pump { 60.0 } else { 1800.0 },
            dt,
        );
        self.firing_pct = added / capacity * 100.0;
        self.out = PlantOut {
            supply_t: self.supply_t,
            flowing: pump,
        };

        sense(db, m, p.hws, self.supply_t);
        sense(db, m, p.hwr, self.return_t);
        sense(db, m, p.fire, self.firing_pct);
        bus.energy.gas_btuh += added / 0.85;
        bus.energy.hvac_kw +=
            if pump { self.gpm * 0.015 } else { 0.0 } + if self.running { 0.5 } else { 0.0 };
    }

    pub fn headline(&self) -> String {
        match (self.running, self.out.flowing) {
            (true, _) => format!(
                "Firing {:.0} % · HWS {}",
                self.firing_pct,
                fmt_f(self.supply_t)
            ),
            (false, true) => format!("Pump only · HWS {}", fmt_f(self.supply_t)),
            _ => format!("Off · HWS {}", fmt_f(self.supply_t)),
        }
    }
}
