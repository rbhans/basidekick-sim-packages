//! The smaller controllers: the site controller (router, weather station,
//! building occupancy), exhaust fans, lighting relays, the electric meter
//! and gas unit heaters.

use super::*;
use crate::control::{Delay, Stager};
use crate::package::{FanDef, Heating, Hours, ScheduleDef, UnitHeaterDef};
use crate::zone::Zone;
use chrono::{Datelike, NaiveTime};

pub struct Site {
    pub dev: usize,
    oa_t: PointRef,
    oa_rh: PointRef,
    oa_dp: PointRef,
    occ_mode: PointRef,
    occ: PointRef,
    people: PointRef,
    fire: PointRef,
    /// The building's occupancy schedule, a BACnet schedule object a BAS
    /// can read and edit.
    pub schedule: PointRef,
    pub occupied: bool,
}

impl Site {
    pub fn new(b: &mut DevBuilder, hours: &ScheduleDef) -> Self {
        let day = |h: Option<Hours>| -> Vec<(NaiveTime, bool)> {
            let at = |h: f64| {
                NaiveTime::from_num_seconds_from_midnight_opt(
                    (h * 3600.0).round() as u32 % 86_400,
                    0,
                )
                .unwrap_or_default()
            };
            h.map(|h| vec![(at(h.open), true), (at(h.close), false)])
                .unwrap_or_default()
        };
        let weekday = day(hours.weekday);
        let weekly = [
            weekday.clone(),
            weekday.clone(),
            weekday.clone(),
            weekday.clone(),
            weekday,
            day(hours.saturday),
            day(hours.sunday),
        ];
        let mut schedule = bask_points::BacObject::schedule(1, "OCC-SCHEDULE", weekly);
        schedule.description =
            "Occupancy schedule: the HVAC is occupied while it's On (optimal start brings it on early)".into();
        Site {
            dev: b.device(),
            schedule: b.object(schedule),
            oa_t: b.ai(
                "OA-T",
                "Outside air temperature (weather station)",
                Units::DegreesFahrenheit,
                60.0,
            ),
            oa_rh: b.ai(
                "OA-RH",
                "Outside air relative humidity",
                Units::PercentRelativeHumidity,
                50.0,
            ),
            oa_dp: b.ai(
                "OA-DP",
                "Outside air dew point",
                Units::DegreesFahrenheit,
                45.0,
            ),
            occ_mode: b.msv(
                "BLDG-OCC-MODE",
                "Building occupancy command (Auto follows the schedule)",
                &OCC_MODES,
                1,
                true,
            ),
            occ: b.bv(
                "BLDG-OCC",
                "Building occupied mode",
                ["Unoccupied", "Occupied"],
            ),
            people: b.ai(
                "PEOPLE-CNT",
                "People count (entrance counters)",
                Units::NoUnits,
                0.0,
            ),
            fire: b.alarm("FIRE-ALM", "Fire alarm panel in alarm"),
            occupied: false,
        }
    }

    /// Resolves the building's occupied mode from the schedule and any
    /// override written to `BLDG-OCC-MODE`.
    pub fn step(
        &mut self,
        db: &mut PointDb,
        mods: &Mods,
        weather: &Weather,
        scheduled: bool,
        people: f64,
    ) -> bool {
        sense(db, mods, self.oa_t, weather.oat);
        sense(db, mods, self.oa_rh, weather.rh);
        sense(db, mods, self.oa_dp, weather.dew_point());
        sense(db, mods, self.people, people.round());
        alarm(db, self.fire, mods.fire);
        self.occupied = occupied(db.read_u32(self.occ_mode), scheduled || mods.force_occupied);
        db.set_bool(self.occ, self.occupied);
        self.occupied
    }
}

pub struct ExhaustFan {
    pub dev: usize,
    pub id: String,
    cfm: f64,
    /// The interlocked unit's fan status point.
    pub interlock: Option<PointRef>,
    ef_c: PointRef,
    ef_s: PointRef,
    fail: PointRef,
    delay: Delay,
    pub running: bool,
}

impl ExhaustFan {
    pub fn new(def: &FanDef, interlock: Option<PointRef>, b: &mut DevBuilder) -> Self {
        ExhaustFan {
            dev: b.device(),
            id: def.common.id.clone(),
            cfm: def.cfm,
            interlock,
            ef_c: b.bo("EF-C", "Exhaust fan command", ["Off", "On"]),
            ef_s: b.bi("EF-S", "Exhaust fan status", ["Off", "On"]),
            fail: b.alarm("EF-FAIL", "Exhaust fan failure: commanded on, no status"),
            delay: Delay::default(),
            running: false,
        }
    }

    pub fn step(&mut self, env: &Env, db: &mut PointDb, bus: &mut Bus) {
        let m = env.mods;
        let want = match self.interlock {
            Some(r) => db.read_bool(r),
            None => env.occupied,
        };
        let cmd = program_bool(db, self.ef_c, want);
        self.running = cmd && !m.fail_fan[self.dev] && !m.shutdown();
        status(db, self.ef_s, self.running);
        let a = self
            .delay
            .update(cmd && !self.running && !m.shutdown(), 30.0, env.dt);
        alarm(db, self.fail, a);
        if self.running {
            bus.energy.hvac_kw += self.cfm * 0.0004;
        }
    }

    pub fn headline(&self) -> String {
        if self.running {
            format!("Running · {:.0} cfm", self.cfm)
        } else {
            "Off".into()
        }
    }
}

pub struct Lighting {
    pub dev: usize,
    pub id: String,
    pub relays: Vec<(usize, PointRef)>,
    kw: PointRef,
    pub on: usize,
}

impl Lighting {
    pub fn new(id: &str, zones: &[(usize, &Zone)], b: &mut DevBuilder) -> Self {
        let relays = zones
            .iter()
            .map(|(i, z)| {
                (
                    *i,
                    b.bo(
                        &format!("LTG-{}", point_tag(&z.id)),
                        &format!("Lighting relay, {}", z.name),
                        ["Off", "On"],
                    ),
                )
            })
            .collect();
        Lighting {
            dev: b.device(),
            id: id.into(),
            relays,
            kw: b.av(
                "LTG-KW",
                "Lighting power on this panel",
                Units::Kilowatts,
                0.0,
            ),
            on: 0,
        }
    }

    /// Sets each zone's lights from its relay.
    pub fn step(&mut self, env: &Env, db: &mut PointDb, zones: &mut [Zone]) {
        let mut kw = 0.0;
        self.on = 0;
        for &(z, r) in &self.relays {
            let lit = program_bool(db, r, env.lights) && !env.mods.power_out;
            zones[z].lights_on = lit;
            kw += zones[z].lights_kw();
            self.on += lit as usize;
        }
        db.set_real(self.kw, kw);
    }

    pub fn headline(&self) -> String {
        format!("{} of {} circuits on", self.on, self.relays.len())
    }
}

pub struct Meter {
    pub dev: usize,
    pub id: String,
    kw: PointRef,
    kwh: PointRef,
    peak: PointRef,
    hvac: PointRef,
    lights: PointRef,
    plugs: PointRef,
    gas_rate: PointRef,
    gas_total: PointRef,
    day: u32,
    pub total_kwh: f64,
    pub peak_kw: f64,
    pub therms: f64,
    pub last_kw: f64,
}

impl Meter {
    pub fn new(id: &str, b: &mut DevBuilder) -> Self {
        Meter {
            dev: b.device(),
            id: id.into(),
            kw: b.ai("KW", "Building electric demand", Units::Kilowatts, 0.0),
            kwh: b.ai(
                "KWH",
                "Building electric consumption",
                Units::KilowattHours,
                0.0,
            ),
            peak: b.ai("PEAK-KW", "Today's peak demand", Units::Kilowatts, 0.0),
            hvac: b.ai("HVAC-KW", "HVAC demand (submeter)", Units::Kilowatts, 0.0),
            lights: b.ai(
                "LTG-KW",
                "Lighting demand (submeter)",
                Units::Kilowatts,
                0.0,
            ),
            plugs: b.ai(
                "PLUG-KW",
                "Plug load demand (submeter)",
                Units::Kilowatts,
                0.0,
            ),
            gas_rate: b.ai(
                "GAS-RATE",
                "Natural gas input rate",
                Units::KiloBtusPerHour,
                0.0,
            ),
            gas_total: b.ai("GAS-THM", "Natural gas consumption", Units::Therms, 0.0),
            day: 0,
            total_kwh: 184_250.0,
            peak_kw: 0.0,
            therms: 9_412.0,
            last_kw: 0.0,
        }
    }

    pub fn step(&mut self, env: &Env, db: &mut PointDb, energy: &Energy) {
        let kw = if env.mods.power_out {
            0.0
        } else {
            energy.total_kw()
        };
        let day = env.now.ordinal();
        if day != self.day {
            self.day = day;
            self.peak_kw = 0.0;
        }
        self.total_kwh += kw * env.dt / 3600.0;
        self.therms += energy.gas_btuh * env.dt / 3600.0 / 100_000.0;
        self.peak_kw = self.peak_kw.max(kw);
        self.last_kw = kw;
        let m = env.mods;
        sense(db, m, self.kw, kw);
        sense(db, m, self.kwh, self.total_kwh.floor());
        sense(db, m, self.peak, self.peak_kw);
        sense(db, m, self.hvac, energy.hvac_kw);
        sense(db, m, self.lights, energy.lighting_kw);
        sense(db, m, self.plugs, energy.plug_kw);
        sense(db, m, self.gas_rate, energy.gas_btuh / 1000.0);
        sense(db, m, self.gas_total, self.therms.floor());
    }

    pub fn headline(&self) -> String {
        format!("{:.1} kW · peak {:.1} kW", self.last_kw, self.peak_kw)
    }
}

pub struct UnitHeater {
    pub dev: usize,
    pub id: String,
    pub zone: usize,
    mbh: f64,
    heating: Heating,
    zn_t: PointRef,
    uh_c: PointRef,
    uh_s: PointRef,
    occ_htg: PointRef,
    unocc_htg: PointRef,
    occ_mode: PointRef,
    occ: PointRef,
    fail: PointRef,
    diff: PointRef,
    stage: Stager,
    delay: Delay,
    pub running: bool,
    pub heating_now: bool,
}

impl UnitHeater {
    pub fn new(def: &UnitHeaterDef, zone: usize, b: &mut DevBuilder) -> Self {
        UnitHeater {
            dev: b.device(),
            id: def.common.id.clone(),
            zone,
            mbh: def.mbh,
            heating: def.heating,
            zn_t: b.ai("ZN-T", "Space temperature", Units::DegreesFahrenheit, 60.0),
            uh_c: b.bo(
                "UH-C",
                "Unit heater command (fan and burner)",
                ["Off", "On"],
            ),
            uh_s: b.bi("UH-S", "Unit heater fan status", ["Off", "On"]),
            occ_htg: b.sp(
                "OCC-HTG-SP",
                "Occupied heating setpoint",
                Units::DegreesFahrenheit,
                58.0,
                40.0,
                80.0,
            ),
            unocc_htg: b.sp(
                "UNOCC-HTG-SP",
                "Unoccupied heating setpoint",
                Units::DegreesFahrenheit,
                50.0,
                35.0,
                70.0,
            ),
            occ_mode: b.occ_mode(),
            occ: b.occ_status(),
            fail: b.alarm(
                "UH-FAIL",
                "Unit heater failure: commanded on, no fan status",
            ),
            diff: b.param(
                "HTG-DIFF",
                "Thermostat differential, split around setpoint",
                Units::DeltaDegreesFahrenheit,
                2.0,
                0.2,
                10.0,
            ),
            stage: Stager::new(1, 180.0, 180.0),
            delay: Delay::default(),
            running: false,
            heating_now: false,
        }
    }

    pub fn step(&mut self, env: &Env, db: &mut PointDb, zones: &[Zone], bus: &mut Bus) {
        let m = env.mods;
        sense(db, m, self.zn_t, zones[self.zone].temp);
        let occ = occupied(db.read_u32(self.occ_mode), env.occupied);
        db.set_bool(self.occ, occ);
        let sp = db.read_f64(if occ { self.occ_htg } else { self.unocc_htg });
        let t = db.read_f64(self.zn_t);
        // on half the differential below setpoint, off half above
        let half = db.read_f64(self.diff) / 2.0;
        let demand = if self.stage.on(0) {
            if t < sp + half { 100.0 } else { 0.0 }
        } else if t < sp - half {
            100.0
        } else {
            0.0
        };
        self.stage.update(demand, true, env.dt);
        let cmd = program_bool(db, self.uh_c, self.stage.on(0));
        self.running = cmd && !m.fail_fan[self.dev] && !m.shutdown();
        status(db, self.uh_s, self.running);
        let a = self
            .delay
            .update(cmd && !self.running && !m.shutdown(), 30.0, env.dt);
        alarm(db, self.fail, a);
        self.heating_now = self.running && !m.fail_heat[self.dev];
        let air = &mut bus.air[self.zone];
        air.setpoints = air.setpoints.or(Some((95.0, sp)));
        if self.heating_now {
            let out = self.mbh * 1000.0 * 0.8;
            air.heat_btuh += out;
            if self.heating == Heating::Electric {
                bus.energy.hvac_kw += out / 3412.0;
            } else {
                bus.energy.gas_btuh += self.mbh * 1000.0;
            }
        }
        if self.running {
            bus.energy.hvac_kw += 0.4;
        }
    }

    pub fn headline(&self) -> String {
        match (self.running, self.heating_now) {
            (true, true) => "Heating".into(),
            (true, false) => "Fan on, no heat".into(),
            _ => "Off".into(),
        }
    }
}
