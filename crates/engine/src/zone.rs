//! A zone as one lump of air and furniture: envelope losses, sun, people,
//! lights and plugs on one side, whatever the HVAC blows in on the other.

use crate::package::ZoneDef;
use crate::world::{Weather, facing_factor};

pub const OUTDOOR_CO2: f64 = 420.0;
/// Sensible heat per person, BTU/h.
const PERSON_BTUH: f64 = 250.0;

/// What the HVAC delivers to a zone in one step.
#[derive(Clone, Debug, Default)]
pub struct ZoneAir {
    pub cfm: f64,
    /// Σ cfm × supply temperature, to mix several supplies.
    pub cfm_t: f64,
    pub oa_cfm: f64,
    /// Heat put straight in (unit heaters), BTU/h.
    pub heat_btuh: f64,
    /// The serving controller's effective (cooling, heating) setpoints.
    pub setpoints: Option<(f64, f64)>,
}

impl ZoneAir {
    pub fn supply(&mut self, cfm: f64, temp: f64, oa_fraction: f64) {
        self.cfm += cfm;
        self.cfm_t += cfm * temp;
        self.oa_cfm += cfm * oa_fraction;
    }
}

#[derive(Clone, Debug)]
pub struct Zone {
    pub id: String,
    pub name: String,
    pub area: f64,
    pub peak_people: f64,
    facing: String,
    lights_w: f64,
    plug_w: f64,
    process_btuh: f64,
    door_cfm: f64,
    co2_per_person: f64,
    /// Envelope UA, BTU/h·°F.
    ua: f64,
    /// Thermal capacitance, BTU/°F.
    capacitance: f64,
    volume: f64,
    solar_peak_btuh: f64,
    infiltration_cfm: f64,

    pub temp: f64,
    pub co2: f64,
    pub people: f64,
    pub lights_on: bool,
    /// Last step's delivered air, for display.
    pub supply_cfm: f64,
    pub supply_temp: f64,
    pub setpoints: Option<(f64, f64)>,
    pub door_open: bool,
    /// Controllers that serve it, for display.
    pub served_by: Vec<String>,
}

impl Zone {
    pub fn new(def: &ZoneDef) -> Self {
        let exposure = def.exposure.clamp(0.0, 1.5);
        // no two rooms are quite alike: vary each a little, the same way every run
        let vary = |salt: u64, pct: f64| 1.0 + pct * spread(&def.id, salt);
        Zone {
            id: def.id.clone(),
            name: def.name.clone(),
            area: def.area_sqft,
            peak_people: (def.people * vary(1, 0.12)).round(),
            facing: def.facing.clone(),
            lights_w: def.lights_w_sqft * def.area_sqft,
            plug_w: def.plug_w_sqft * def.area_sqft * vary(2, 0.2),
            process_btuh: def.process_btuh,
            door_cfm: def.door_cfm,
            co2_per_person: def.co2_cfm_per_person,
            ua: def.area_sqft * (0.03 + 0.17 * exposure) * vary(3, 0.1),
            capacitance: def.area_sqft * 2.5 * (def.height_ft / 10.0).max(0.5) * vary(4, 0.15),
            volume: def.area_sqft * def.height_ft,
            solar_peak_btuh: def.area_sqft * 14.0 * exposure,
            infiltration_cfm: def.area_sqft * 0.02 * exposure,
            temp: def.start_temp_f + spread(&def.id, 5),
            co2: 450.0,
            people: 0.0,
            lights_on: false,
            supply_cfm: 0.0,
            supply_temp: def.start_temp_f,
            setpoints: None,
            door_open: false,
            served_by: Vec::new(),
        }
    }

    /// Integrate one step. `people_share` is the share of peak occupancy,
    /// `plugs` the share of plug load that's drawing.
    #[allow(clippy::too_many_arguments)]
    pub fn step(
        &mut self,
        dt: f64,
        hour: f64,
        weather: &Weather,
        air: &ZoneAir,
        people_share: f64,
        plugs: f64,
        extra_gain_btuh: f64,
        door_cfm: f64,
    ) {
        self.people = self.peak_people * people_share;
        self.door_open = door_cfm > 0.0;
        let t = self.temp;
        let leak_cfm = self.infiltration_cfm + door_cfm;
        let q_envelope = self.ua * (weather.oat - t) + 1.08 * leak_cfm * (weather.oat - t);
        let q_sun = self.solar_peak_btuh * facing_factor(&self.facing, hour) * weather.sun;
        let q_lights = if self.lights_on {
            self.lights_w * 3.412
        } else {
            0.0
        };
        let q_internal = self.people * PERSON_BTUH
            + q_lights
            + self.plug_w * plugs * 3.412
            + self.process_btuh
            + extra_gain_btuh;
        let q_air = 1.08 * (air.cfm_t - air.cfm * t);
        let q = q_envelope + q_sun + q_internal + q_air + air.heat_btuh;
        self.temp += q * dt / 3600.0 / self.capacitance;

        // CO2 in ppm: people breathe it out, outside air flushes it
        let oa = air.oa_cfm + leak_cfm;
        let dc =
            (self.people * self.co2_per_person * 1e6 - oa * (self.co2 - OUTDOOR_CO2)) / self.volume;
        self.co2 = (self.co2 + dc * dt / 60.0).max(OUTDOOR_CO2);

        self.supply_cfm = air.cfm;
        if air.cfm > 1.0 {
            self.supply_temp = air.cfm_t / air.cfm;
        }
        self.setpoints = air.setpoints;
    }

    pub fn lights_kw(&self) -> f64 {
        if self.lights_on {
            self.lights_w / 1000.0
        } else {
            0.0
        }
    }

    pub fn plug_kw(&self, plugs: f64) -> f64 {
        self.plug_w * plugs / 1000.0
    }

    pub fn door_cfm(&self) -> f64 {
        self.door_cfm
    }
}

/// A stable value in -1..1 from a zone id.
fn spread(id: &str, salt: u64) -> f64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ salt;
    for b in id.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
    }
    (h % 2001) as f64 / 1000.0 - 1.0
}
