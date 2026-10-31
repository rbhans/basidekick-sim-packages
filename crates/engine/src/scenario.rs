//! Scenarios: things that happen to a building. Each is a list of effects
//! that fade in over `ramp_min`, last `duration_min` of sim time (or until
//! stopped), and fade back out.

use bask_points::PointRef;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioDef {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: Category,
    /// Sim minutes; none runs until stopped.
    #[serde(default)]
    pub duration_min: Option<f64>,
    #[serde(default)]
    pub ramp_min: f64,
    pub effects: Vec<Effect>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Weather,
    Occupancy,
    #[default]
    Equipment,
    Network,
    Emergency,
}

impl Category {
    pub const ALL: [Category; 5] = [
        Category::Weather,
        Category::Occupancy,
        Category::Equipment,
        Category::Network,
        Category::Emergency,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Weather => "Weather",
            Category::Occupancy => "Occupancy",
            Category::Equipment => "Equipment",
            Category::Network => "Network",
            Category::Emergency => "Emergency",
        }
    }
}

/// The part of a unit that fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Part {
    /// Supply / exhaust fan, unit heater fan, or a plant's pump.
    Fan,
    /// Compressors or the chiller.
    Cooling,
    /// Burner, heating coil, reheat or the boiler.
    Heating,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actuator {
    /// A VAV box's damper.
    Damper,
    OaDamper,
    CoolingValve,
    HeatingValve,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorMode {
    /// Stops updating.
    Frozen,
    /// Reads `value` off (grows in over the ramp, so a long ramp is a drift).
    Offset,
    /// Open circuit: reads `value` (default -40) and reports a fault.
    Fail,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Effect {
    OutdoorTemp {
        delta_f: f64,
    },
    /// Scale people in the building (or in some zones), and/or keep at
    /// least `floor` of peak there regardless of the schedule.
    People {
        #[serde(default = "one")]
        factor: f64,
        #[serde(default)]
        floor: f64,
        #[serde(default)]
        zones: Vec<String>,
    },
    /// Every unit runs occupied, whatever the schedule says.
    ForceOccupied,
    HeatGain {
        zone: String,
        btuh: f64,
    },
    DoorOpen {
        zone: String,
        #[serde(default)]
        cfm: Option<f64>,
    },
    Fail {
        equip: String,
        part: Part,
    },
    Stuck {
        equip: String,
        actuator: Actuator,
        position: f64,
    },
    Sensor {
        equip: String,
        point: String,
        mode: SensorMode,
        #[serde(default)]
        value: Option<f64>,
    },
    /// Adds this much pressure drop across the unit's filter.
    Filter {
        equip: String,
        dp: f64,
    },
    /// The controller stops answering BACnet. `*` is every controller.
    Offline {
        equip: String,
    },
    PowerOutage,
    FireAlarm,
}

fn one() -> f64 {
    1.0
}

impl Effect {
    pub fn equipment(&self) -> Option<&str> {
        match self {
            Effect::Fail { equip, .. }
            | Effect::Stuck { equip, .. }
            | Effect::Sensor { equip, .. }
            | Effect::Filter { equip, .. }
            | Effect::Offline { equip } => Some(equip),
            _ => None,
        }
    }

    pub fn zone(&self) -> Option<&str> {
        match self {
            Effect::HeatGain { zone, .. } | Effect::DoorOpen { zone, .. } => Some(zone),
            _ => None,
        }
    }
}

/// Scenarios every building gets.
pub fn global_scenarios() -> Vec<ScenarioDef> {
    let s = |id: &str, name: &str, category, duration_min, ramp_min, description: &str, effects| {
        ScenarioDef {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            category,
            duration_min,
            ramp_min,
            effects,
        }
    };
    vec![
        s(
            "heat-wave",
            "Heat wave",
            Category::Weather,
            Some(480.0),
            90.0,
            "Outdoor air climbs 16 °F over an hour and a half and stays there for eight hours. Economizers lock out, cooling runs flat out, west and south zones drift up.",
            vec![Effect::OutdoorTemp { delta_f: 16.0 }],
        ),
        s(
            "cold-snap",
            "Cold snap",
            Category::Weather,
            Some(480.0),
            90.0,
            "Outdoor air drops 28 °F. Mixed air falls, heating plants work hard and perimeter zones sag.",
            vec![Effect::OutdoorTemp { delta_f: -28.0 }],
        ),
        s(
            "after-hours",
            "After-hours event",
            Category::Occupancy,
            Some(180.0),
            0.0,
            "A late meeting: everything runs occupied and the building is two-thirds full for three hours, whatever the schedule says.",
            vec![
                Effect::ForceOccupied,
                Effect::People {
                    factor: 1.0,
                    floor: 0.65,
                    zones: vec![],
                },
            ],
        ),
        s(
            "full-house",
            "Packed building",
            Category::Occupancy,
            Some(120.0),
            15.0,
            "50 % more people than the design count for two hours. Cooling demand and CO2 climb everywhere.",
            vec![Effect::People {
                factor: 1.5,
                floor: 0.0,
                zones: vec![],
            }],
        ),
        s(
            "network-outage",
            "Network outage",
            Category::Network,
            Some(15.0),
            0.0,
            "Every controller drops off BACnet for 15 minutes. The equipment keeps running on its own logic; the BAS sees nothing but timeouts.",
            vec![Effect::Offline { equip: "*".into() }],
        ),
        s(
            "fire-alarm",
            "Fire alarm",
            Category::Emergency,
            Some(30.0),
            0.0,
            "The fire alarm shuts every fan down through its hardwired relay and the building empties. Commands stay on, statuses drop.",
            vec![Effect::FireAlarm],
        ),
        s(
            "power-outage",
            "Power outage",
            Category::Emergency,
            Some(20.0),
            0.0,
            "Utility power drops for 20 minutes: every unit stops and every controller goes dark, then they all come back at once.",
            vec![Effect::PowerOutage],
        ),
    ]
}

/// A scenario that's running, or fading out after being stopped.
#[derive(Clone, Debug)]
pub struct Active {
    pub def: ScenarioDef,
    /// Sim seconds since it started.
    pub elapsed_s: f64,
    /// Sim seconds since it was stopped, while it fades out.
    pub ending_s: Option<f64>,
    /// Device indexes the effects resolved to, and sensor points.
    pub resolved: Vec<Resolved>,
}

#[derive(Clone, Debug)]
pub enum Resolved {
    Devices(Vec<usize>),
    Point(Option<PointRef>),
    Zones(Vec<usize>),
    Nothing,
}

impl Active {
    /// How strongly its effects apply right now, 0..1.
    pub fn scale(&self) -> f64 {
        let ramp = self.def.ramp_min * 60.0;
        let up = if ramp > 0.0 {
            (self.elapsed_s / ramp).min(1.0)
        } else {
            1.0
        };
        match self.ending_s {
            Some(t) if ramp > 0.0 => (up * (1.0 - t / ramp)).max(0.0),
            Some(_) => 0.0,
            None => up,
        }
    }

    pub fn remaining_s(&self) -> Option<f64> {
        self.def
            .duration_min
            .map(|d| (d * 60.0 - self.elapsed_s).max(0.0))
    }

    pub fn finished(&self) -> bool {
        self.ending_s.is_some() && self.scale() <= 0.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SensorFault {
    Frozen,
    Offset(f64),
    Fail(f64),
}

/// Everything active scenarios do this step, gathered for the equipment.
#[derive(Clone, Debug, Default)]
pub struct Mods {
    pub oat_delta: f64,
    /// Per zone.
    pub people_factor: Vec<f64>,
    pub people_floor: Vec<f64>,
    pub gain_btuh: Vec<f64>,
    pub door_cfm: Vec<f64>,
    pub force_occupied: bool,
    /// Per device.
    pub fail_fan: Vec<bool>,
    pub fail_cool: Vec<bool>,
    pub fail_heat: Vec<bool>,
    pub filter_dp: Vec<f64>,
    pub offline: Vec<bool>,
    pub stuck: HashMap<(usize, Actuator), f64>,
    pub sensors: HashMap<PointRef, SensorFault>,
    pub power_out: bool,
    pub fire: bool,
}

impl Mods {
    pub fn new(zones: usize, devices: usize) -> Self {
        Mods {
            people_factor: vec![1.0; zones],
            people_floor: vec![0.0; zones],
            gain_btuh: vec![0.0; zones],
            door_cfm: vec![0.0; zones],
            fail_fan: vec![false; devices],
            fail_cool: vec![false; devices],
            fail_heat: vec![false; devices],
            filter_dp: vec![0.0; devices],
            offline: vec![false; devices],
            ..Default::default()
        }
    }

    pub fn stuck(&self, device: usize, actuator: Actuator) -> Option<f64> {
        self.stuck.get(&(device, actuator)).copied()
    }

    /// Fans, pumps and burners can't run: fire alarm or no power.
    pub fn shutdown(&self) -> bool {
        self.fire || self.power_out
    }
}
