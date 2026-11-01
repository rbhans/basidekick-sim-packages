//! A copy of everything the UI shows, taken under the locks and then read
//! without them.

use crate::equip::fmt_f;
use crate::scenario::Category;
use crate::sim::{Kind, LogEntry, Simulation};
use crate::world::{Season, Weather};
use chrono::NaiveDateTime;

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub package_id: String,
    pub building: String,
    pub network: u16,
    pub now: NaiveDateTime,
    pub speed: f64,
    pub paused: bool,
    pub season: Season,
    pub weather: Weather,
    pub occupied: bool,
    /// When people are due in and out today.
    pub hours: Option<(f64, f64)>,
    /// When the BACnet occupancy schedule is on today. It starts out the
    /// same as `hours`; a BAS can edit it.
    pub schedule: Option<(f64, f64)>,
    pub people: f64,
    pub kw: f64,
    pub kwh: f64,
    pub zones: Vec<ZoneSnap>,
    pub devices: Vec<DeviceSnap>,
    pub scenarios: Vec<ScenarioSnap>,
    pub log: Vec<LogEntry>,
}

#[derive(Clone, Debug)]
pub struct ZoneSnap {
    pub name: String,
    pub temp: f64,
    pub co2: f64,
    /// Relative humidity, %.
    pub rh: f64,
    pub people: f64,
    pub setpoints: Option<(f64, f64)>,
    pub supply_cfm: f64,
    pub supply_temp: f64,
    pub lights_on: bool,
    pub door_open: bool,
    pub served_by: String,
}

impl ZoneSnap {
    /// Degrees outside the setpoint band: negative cold, positive warm.
    pub fn off_band(&self) -> f64 {
        match self.setpoints {
            Some((clg, _)) if self.temp > clg => self.temp - clg,
            Some((_, htg)) if self.temp < htg => self.temp - htg,
            _ => 0.0,
        }
    }

    pub fn band_text(&self) -> String {
        match self.setpoints {
            Some((clg, htg)) if clg >= 95.0 => format!("heat to {}", fmt_f(htg)),
            Some((clg, htg)) => format!("{:.0}–{:.0}°F", htg, clg),
            None => "unconditioned".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DeviceSnap {
    pub index: usize,
    pub name: String,
    pub description: String,
    pub instance: u32,
    pub mac: Option<u8>,
    pub kind: Kind,
    pub online: bool,
    pub alarms: usize,
    pub faults: usize,
    pub overrides: usize,
    pub headline: String,
    pub points: Vec<PointSnap>,
}

#[derive(Clone, Debug)]
pub struct PointSnap {
    pub name: String,
    pub kind: &'static str,
    pub instance: u32,
    pub value: String,
    pub description: String,
    /// Priority holding a BAS command, when one does.
    pub priority: Option<u8>,
    pub out_of_service: bool,
    pub fault: bool,
    pub alarm: bool,
    /// How the app may change it, if at all.
    pub write: Write,
    /// Binary and multi-state: what each value means, in order.
    pub states: Vec<String>,
    /// Analog: the present value as a number, for editing.
    pub number: Option<f64>,
}

/// How a point can be changed from the app, the way an operator would.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Write {
    ReadOnly,
    /// Commandable: written at priority 8, released to the program.
    Command,
    /// A writable setting: takes the value, no priority.
    Setting,
    /// A sensor: taken out of service at a value, put back in service.
    Sensor,
}

#[derive(Clone, Debug)]
pub struct ScenarioSnap {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: Category,
    pub duration_min: Option<f64>,
    /// Running (or fading out).
    pub active: bool,
    pub ending: bool,
    pub scale: f64,
    pub remaining_s: Option<f64>,
    /// Not in the catalog: a quick or mystery fault.
    pub quick: bool,
    /// For a mystery fault, what it really is (the UI hides it until asked).
    pub answer: Option<String>,
}

impl Simulation {
    pub fn snapshot(&self) -> Snapshot {
        let db = self.db.lock();
        let devices = db
            .devices
            .iter()
            .enumerate()
            .map(|(i, d)| DeviceSnap {
                index: i,
                name: d.name.clone(),
                description: d.description.clone(),
                instance: d.instance,
                mac: d.mac,
                kind: self.kind(i),
                online: d.online,
                alarms: d.objects.iter().filter(|o| o.in_alarm).count(),
                faults: d.objects.iter().filter(|o| o.fault()).count(),
                overrides: d
                    .objects
                    .iter()
                    .filter(|o| o.is_overridden() || o.out_of_service)
                    .count(),
                headline: self.headline(i),
                points: d
                    .objects
                    .iter()
                    .filter(|o| o.object_type.has_present_value())
                    .map(|o| PointSnap {
                        name: o.name.clone(),
                        kind: o.object_type.abbrev(),
                        instance: o.instance,
                        value: o.display_value(),
                        description: o.description.clone(),
                        priority: o.active_priority(),
                        out_of_service: o.out_of_service,
                        fault: o.fault(),
                        alarm: o.in_alarm,
                        write: if o.commandable {
                            Write::Command
                        } else if o.object_type.is_input() {
                            Write::Sensor
                        } else if o.writable {
                            Write::Setting
                        } else {
                            Write::ReadOnly
                        },
                        states: o.states.clone(),
                        number: o.object_type.is_analog().then(|| o.present_value.as_f64()),
                    })
                    .collect(),
            })
            .collect();
        drop(db);

        let mut scenarios: Vec<ScenarioSnap> = self
            .catalog
            .iter()
            .map(|s| {
                let a = self.active.iter().find(|a| a.def.id == s.id);
                ScenarioSnap {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    description: s.description.clone(),
                    category: s.category,
                    duration_min: s.duration_min,
                    active: a.is_some(),
                    ending: a.is_some_and(|a| a.ending_s.is_some()),
                    scale: a.map(|a| a.scale()).unwrap_or(0.0),
                    remaining_s: a.and_then(|a| a.remaining_s()),
                    quick: false,
                    answer: None,
                }
            })
            .collect();
        for a in self
            .active
            .iter()
            .filter(|a| !self.catalog.iter().any(|s| s.id == a.def.id))
        {
            scenarios.push(ScenarioSnap {
                id: a.def.id.clone(),
                name: a.def.name.clone(),
                description: a.def.description.clone(),
                category: a.def.category,
                duration_min: a.def.duration_min,
                active: true,
                ending: a.ending_s.is_some(),
                scale: a.scale(),
                remaining_s: a.remaining_s(),
                quick: true,
                answer: self.mystery_answer(&a.def.id).map(String::from),
            });
        }

        Snapshot {
            package_id: self.package.id.clone(),
            building: self.package.name.clone(),
            network: self.package.network,
            now: self.now,
            speed: self.speed,
            paused: self.paused,
            season: self.season,
            weather: self.weather,
            occupied: self.occupied,
            hours: self.schedule_hours(),
            schedule: self.schedule_today(),
            people: self.people,
            kw: self.energy.total_kw(),
            kwh: self.kwh(),
            zones: self
                .zones
                .iter()
                .map(|z| ZoneSnap {
                    name: z.name.clone(),
                    temp: z.temp,
                    co2: z.co2,
                    rh: z.rh(),
                    people: z.people,
                    setpoints: z.setpoints,
                    supply_cfm: z.supply_cfm,
                    supply_temp: z.supply_temp,
                    lights_on: z.lights_on,
                    door_open: z.door_open,
                    served_by: z.served_by.join(", "),
                })
                .collect(),
            devices,
            scenarios,
            log: self.log.iter().cloned().collect(),
        }
    }
}
