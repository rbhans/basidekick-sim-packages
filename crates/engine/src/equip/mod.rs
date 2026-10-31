//! The simulated controllers. Each one is a BACnet device: it reads its
//! sensors from the physics, runs a plain DDC sequence, writes its outputs
//! through the object database (so a BAS override wins), then acts on
//! whatever the outputs ended up being.

mod ahu;
mod misc;
mod plant;
mod rtu;
mod vav;

pub use ahu::{Ahu, AhuOut};
pub use misc::{ExhaustFan, Lighting, Meter, Site, UnitHeater};
pub use plant::{BoilerPlant, ChillerPlant, PlantDemand, PlantOut};
pub use rtu::Rtu;
pub use vav::Vav;

use crate::scenario::{Mods, SensorFault};
use crate::world::Weather;
use crate::zone::ZoneAir;
use bask_points::{
    BacDevice, BacObject, ObjectType, PointDb, PointRef, PointValue, Reliability, Units,
};
use chrono::NaiveDateTime;
use std::collections::HashMap;

/// What every controller knows about this step.
pub struct Env<'a> {
    pub dt: f64,
    pub now: NaiveDateTime,
    pub weather: Weather,
    /// The building's occupied mode: schedule, site override and scenarios.
    pub occupied: bool,
    pub lights: bool,
    pub mods: &'a Mods,
}

#[derive(Clone, Debug, Default)]
pub struct Energy {
    pub hvac_kw: f64,
    pub lighting_kw: f64,
    pub plug_kw: f64,
    pub gas_btuh: f64,
}

impl Energy {
    pub fn total_kw(&self) -> f64 {
        self.hvac_kw + self.lighting_kw + self.plug_kw
    }
}

/// What controllers hand each other within a step.
#[derive(Clone, Debug, Default)]
pub struct Bus {
    pub air: Vec<ZoneAir>,
    pub chw: Vec<PlantOut>,
    pub hw: Vec<PlantOut>,
    /// Coil loads gathered this step, for the plants next step.
    pub chw_demand: Vec<PlantDemand>,
    pub hw_demand: Vec<PlantDemand>,
    pub energy: Energy,
}

pub const OCC_MODES: [&str; 3] = ["Auto", "Occupied", "Unoccupied"];

/// Resolve an `OCC-MODE` command against the building's mode.
pub fn occupied(mode: u32, building: bool) -> bool {
    match mode {
        2 => true,
        3 => false,
        _ => building,
    }
}

/// Write a sensor reading through whatever fault a scenario put on it.
pub fn sense(db: &mut PointDb, mods: &Mods, r: PointRef, value: f64) {
    match mods.sensors.get(&r) {
        Some(SensorFault::Frozen) => {}
        Some(SensorFault::Fail(v)) => {
            db.set_real(r, *v);
            db.set_reliability(r, Reliability::OpenLoop);
        }
        fault => {
            let offset = if let Some(SensorFault::Offset(d)) = fault {
                *d
            } else {
                0.0
            };
            db.set_real(r, value + offset);
            if db.obj(r).fault() {
                db.set_reliability(r, Reliability::NoFaultDetected);
            }
        }
    }
}

/// A reading from a jittery transducer (velocity pressure, duct static):
/// `value` plus up to ±`amplitude`, different every tenth of a second but
/// the same on every run.
pub fn sense_noisy(
    db: &mut PointDb,
    mods: &Mods,
    r: PointRef,
    value: f64,
    amplitude: f64,
    now: NaiveDateTime,
) {
    let tick = now.and_utc().timestamp_millis() / 100;
    let mut h = (tick as u64) ^ ((r.device as u64) << 48) ^ ((r.object as u64) << 32);
    h = h.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= h >> 29;
    let unit = (h % 2001) as f64 / 1000.0 - 1.0;
    sense(db, mods, r, value + amplitude * unit);
}

/// A loop's gain and integral time, as writable points. Students change
/// them from the BAS; the controller picks them up every step.
pub struct Tuning {
    kp: PointRef,
    ti: PointRef,
}

impl Tuning {
    pub fn apply(&self, db: &PointDb, pi: &mut crate::control::Pi) {
        pi.tune(db.read_f64(self.kp), db.read_f64(self.ti));
    }
}

/// A binary status or alarm input.
pub fn status(db: &mut PointDb, r: PointRef, on: bool) {
    db.set_bool(r, on);
}

pub fn alarm(db: &mut PointDb, r: PointRef, on: bool) {
    db.set_bool(r, on);
    db.set_alarm(r, on);
}

pub fn program_bool(db: &mut PointDb, r: PointRef, on: bool) -> bool {
    db.set_program(r, PointValue::Binary(on));
    db.read_bool(r)
}

pub fn program_real(db: &mut PointDb, r: PointRef, v: f64) -> f64 {
    db.set_program(r, PointValue::Real(v as f32));
    db.read_f64(r)
}

pub fn program_multi(db: &mut PointDb, r: PointRef, state: u32) {
    db.set_program(r, PointValue::Multi(state));
}

/// Adds one device and its objects to the database.
pub struct DevBuilder<'a> {
    db: &'a mut PointDb,
    dev: usize,
    next: HashMap<ObjectType, u32>,
}

impl<'a> DevBuilder<'a> {
    pub fn new(
        db: &'a mut PointDb,
        instance: u32,
        name: &str,
        description: &str,
        model: &str,
        mac: Option<u8>,
        location: &str,
    ) -> Self {
        db.devices.push(BacDevice {
            instance,
            name: name.into(),
            description: description.into(),
            location: location.into(),
            model_name: model.into(),
            mac,
            online: true,
            objects: Vec::new(),
            database_revision: 1,
        });
        let dev = db.devices.len() - 1;
        DevBuilder {
            db,
            dev,
            next: HashMap::new(),
        }
    }

    pub fn device(&self) -> usize {
        self.dev
    }

    fn add(
        &mut self,
        object_type: ObjectType,
        name: &str,
        description: &str,
        value: PointValue,
    ) -> (PointRef, &mut BacObject) {
        let n = self.next.entry(object_type).or_insert(1);
        let mut obj = BacObject::new(object_type, *n, name, value);
        *n += 1;
        obj.description = description.into();
        let objects = &mut self.db.devices[self.dev].objects;
        objects.push(obj);
        let r = PointRef {
            device: self.dev as u16,
            object: objects.len() as u16 - 1,
        };
        (r, objects.last_mut().unwrap())
    }

    fn analog(
        &mut self,
        t: ObjectType,
        name: &str,
        description: &str,
        units: Units,
        v: f64,
    ) -> (PointRef, &mut BacObject) {
        let (r, o) = self.add(t, name, description, PointValue::Real(v as f32));
        o.units = units;
        o.cov_increment = match units {
            Units::DegreesFahrenheit | Units::DeltaDegreesFahrenheit => 0.2,
            Units::InchesOfWater => 0.02,
            Units::CubicFeetPerMinute => 10.0,
            Units::PartsPerMillion => 10.0,
            Units::KilowattHours | Units::Therms => 1.0,
            _ => 0.5,
        };
        if units == Units::Percent {
            o.min_value = Some(0.0);
            o.max_value = Some(100.0);
        }
        (r, o)
    }

    /// Sensor.
    pub fn ai(&mut self, name: &str, description: &str, units: Units, v: f64) -> PointRef {
        self.analog(ObjectType::AnalogInput, name, description, units, v)
            .0
    }

    /// Modulating output, commandable.
    pub fn ao(&mut self, name: &str, description: &str, units: Units, v: f64) -> PointRef {
        self.analog(ObjectType::AnalogOutput, name, description, units, v)
            .0
    }

    /// Calculated value, read-only.
    pub fn av(&mut self, name: &str, description: &str, units: Units, v: f64) -> PointRef {
        self.analog(ObjectType::AnalogValue, name, description, units, v)
            .0
    }

    /// Setpoint: commandable analog value with a sane write range.
    pub fn sp(
        &mut self,
        name: &str,
        description: &str,
        units: Units,
        v: f64,
        min: f64,
        max: f64,
    ) -> PointRef {
        let (r, o) = self.analog(ObjectType::AnalogValue, name, description, units, v);
        o.commandable = true;
        o.min_value = Some(min as f32);
        o.max_value = Some(max as f32);
        r
    }

    /// Configuration value: writable, not commandable, range-checked.
    pub fn param(
        &mut self,
        name: &str,
        description: &str,
        units: Units,
        v: f64,
        min: f64,
        max: f64,
    ) -> PointRef {
        let (r, o) = self.analog(ObjectType::AnalogValue, name, description, units, v);
        o.writable = true;
        o.min_value = Some(min as f32);
        o.max_value = Some(max as f32);
        o.cov_increment = 0.0;
        r
    }

    /// `<LOOP>-KP` and `<LOOP>-TI` for one PI loop. `per` names the error
    /// units of the gain ("°F", "in. wc", "% of max flow").
    pub fn tuning(
        &mut self,
        loop_tag: &str,
        what: &str,
        per: &str,
        kp: f64,
        ti_s: f64,
        kp_max: f64,
    ) -> Tuning {
        Tuning {
            kp: self.param(
                &format!("{loop_tag}-KP"),
                &format!("{what} loop proportional gain (% output per {per} of error)"),
                Units::NoUnits,
                kp,
                0.0,
                kp_max,
            ),
            ti: self.param(
                &format!("{loop_tag}-TI"),
                &format!("{what} loop integral time (0 = proportional only)"),
                Units::Seconds,
                ti_s,
                0.0,
                7200.0,
            ),
        }
    }

    fn binary(
        &mut self,
        t: ObjectType,
        name: &str,
        description: &str,
        states: [&str; 2],
        v: bool,
    ) -> (PointRef, &mut BacObject) {
        let (r, o) = self.add(t, name, description, PointValue::Binary(v));
        o.states = states.iter().map(|s| s.to_string()).collect();
        (r, o)
    }

    pub fn bi(&mut self, name: &str, description: &str, states: [&str; 2]) -> PointRef {
        self.binary(ObjectType::BinaryInput, name, description, states, false)
            .0
    }

    pub fn alarm(&mut self, name: &str, description: &str) -> PointRef {
        self.bi(name, description, ["Normal", "Alarm"])
    }

    pub fn bo(&mut self, name: &str, description: &str, states: [&str; 2]) -> PointRef {
        self.binary(ObjectType::BinaryOutput, name, description, states, false)
            .0
    }

    /// Calculated binary, read-only.
    pub fn bv(&mut self, name: &str, description: &str, states: [&str; 2]) -> PointRef {
        self.binary(ObjectType::BinaryValue, name, description, states, false)
            .0
    }

    /// Commandable binary value (enables and the like).
    pub fn bv_cmd(
        &mut self,
        name: &str,
        description: &str,
        states: [&str; 2],
        v: bool,
    ) -> PointRef {
        let (r, o) = self.binary(ObjectType::BinaryValue, name, description, states, v);
        o.commandable = true;
        r
    }

    pub fn msv(
        &mut self,
        name: &str,
        description: &str,
        states: &[&str],
        v: u32,
        commandable: bool,
    ) -> PointRef {
        let (r, o) = self.add(
            ObjectType::MultiStateValue,
            name,
            description,
            PointValue::Multi(v),
        );
        o.states = states.iter().map(|s| s.to_string()).collect();
        o.commandable = commandable;
        r
    }

    /// The occupancy command every unit carries.
    pub fn occ_mode(&mut self) -> PointRef {
        self.msv(
            "OCC-MODE",
            "Occupancy command (Auto follows the building schedule)",
            &OCC_MODES,
            1,
            true,
        )
    }

    pub fn occ_status(&mut self) -> PointRef {
        self.bv("OCC", "Occupancy status", ["Unoccupied", "Occupied"])
    }
}

/// `ZONE-ID` from `zone-id`, for point names.
pub fn point_tag(id: &str) -> String {
    id.to_uppercase().replace([' ', '_'], "-")
}

pub fn fmt_f(v: f64) -> String {
    format!("{v:.1}°F")
}
