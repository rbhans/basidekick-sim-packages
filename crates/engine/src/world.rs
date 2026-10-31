//! The world outside the controllers: the sim clock, the weather and when
//! people are in the building.

use crate::package::{Hours, ScheduleDef};
use chrono::{Datelike, NaiveDateTime, Timelike, Weekday};

/// Weather presets. The sim starts in whichever fits today's month.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Season {
    Winter,
    Mild,
    Summer,
}

impl Season {
    pub const ALL: [Season; 3] = [Season::Winter, Season::Mild, Season::Summer];

    pub fn for_month(month: u32) -> Season {
        match month {
            12 | 1 | 2 => Season::Winter,
            6..=8 => Season::Summer,
            _ => Season::Mild,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Season::Winter => "Winter",
            Season::Mild => "Spring / Fall",
            Season::Summer => "Summer",
        }
    }

    /// Typical daily (high, low) °F and mean relative humidity.
    fn climate(self) -> (f64, f64, f64) {
        match self {
            Season::Winter => (34.0, 18.0, 68.0),
            Season::Mild => (66.0, 46.0, 60.0),
            Season::Summer => (91.0, 71.0, 55.0),
        }
    }

    fn sun(self) -> f64 {
        match self {
            Season::Winter => 0.65,
            Season::Mild => 0.85,
            Season::Summer => 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Weather {
    pub oat: f64,
    pub rh: f64,
    /// 0..1, clear-sky sun times cloud cover.
    pub sun: f64,
}

impl Weather {
    pub fn dew_point(&self) -> f64 {
        // Magnus formula, in °C and back
        let t = (self.oat - 32.0) / 1.8;
        let (a, b) = (17.62, 243.12);
        let g = (self.rh.clamp(1.0, 100.0) / 100.0).ln() + a * t / (b + t);
        (b * g / (a - g)) * 1.8 + 32.0
    }
}

/// Hours after midnight, as a fraction.
pub fn hour_of(t: NaiveDateTime) -> f64 {
    t.hour() as f64 + t.minute() as f64 / 60.0 + t.second() as f64 / 3600.0
}

/// Smooth, repeatable noise in -1..1.
fn noise(x: f64, seed: u64) -> f64 {
    fn hash(i: i64, seed: u64) -> f64 {
        let mut h = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ seed.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        h ^= h >> 31;
        h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h ^= h >> 29;
        (h % 20_001) as f64 / 10_000.0 - 1.0
    }
    let i = x.floor();
    let f = x - i;
    let s = f * f * (3.0 - 2.0 * f);
    hash(i as i64, seed) * (1.0 - s) + hash(i as i64 + 1, seed) * s
}

pub fn weather(t: NaiveDateTime, season: Season, oat_delta: f64) -> Weather {
    let (high, low, rh_mean) = season.climate();
    let h = hour_of(t);
    let day = t.date().num_days_from_ce() as f64 + h / 24.0;
    // warmest mid-afternoon, coolest before dawn
    let shape = (std::f64::consts::TAU * (h - 15.0) / 24.0).cos();
    let swing = noise(day / 2.0, 7) * 5.0 + noise(day * 8.0, 11) * 1.2;
    let oat = (high + low) / 2.0 + (high - low) / 2.0 * shape + swing + oat_delta;
    let rh = (rh_mean - 18.0 * shape + noise(day, 13) * 8.0 - oat_delta * 0.8).clamp(15.0, 98.0);
    let cloud = (0.35 + 0.35 * noise(day / 1.5, 17)).clamp(0.0, 0.8);
    let daylight = ((h - 6.0) / 13.0 * std::f64::consts::PI).sin().max(0.0);
    Weather {
        oat,
        rh,
        sun: daylight * season.sun() * (1.0 - cloud),
    }
}

/// How much sun a zone's glass gets at this hour, 0..1.
pub fn facing_factor(facing: &str, h: f64) -> f64 {
    let bell = |peak: f64, width: f64| (-((h - peak) / width).powi(2)).exp();
    match facing {
        "east" => bell(9.0, 2.5),
        "south" => bell(12.5, 3.0),
        "west" => bell(16.0, 2.5),
        "north" => 0.25 * bell(13.0, 4.0),
        "roof" => 0.6 * bell(13.0, 3.5),
        _ => 0.0,
    }
}

pub struct Schedule {
    def: ScheduleDef,
}

impl Schedule {
    pub fn new(def: ScheduleDef) -> Self {
        Schedule { def }
    }

    pub fn hours(&self, t: NaiveDateTime) -> Option<Hours> {
        match t.weekday() {
            Weekday::Sat => self.def.saturday,
            Weekday::Sun => self.def.sunday,
            _ => self.def.weekday,
        }
    }

    /// The shortest warm-up / cool-down lead, minutes before opening.
    pub fn base_lead_min(&self) -> f64 {
        self.def.hvac_lead_min
    }

    /// Lights: a little before opening until a little after closing.
    pub fn lights_on(&self, t: NaiveDateTime) -> bool {
        let h = hour_of(t);
        self.hours(t)
            .is_some_and(|o| h >= o.open - 0.25 && h < o.close + 0.5)
    }

    /// Share of peak people in the building, 0..1.
    pub fn people(&self, t: NaiveDateTime) -> f64 {
        let Some(Hours { open, close }) = self.hours(t) else {
            return 0.0;
        };
        let h = hour_of(t);
        let ramp = |from: f64, to: f64| ((h - from) / (to - from)).clamp(0.0, 1.0);
        if h < open - 0.5 || h > close + 1.0 {
            return 0.0;
        }
        if h >= close {
            // stragglers and cleaners
            return 0.08 * (1.0 - ramp(close, close + 1.0));
        }
        match self.def.profile.as_str() {
            "school" => {
                let arrive = ramp(open - 0.5, open + 0.25);
                let leave = 1.0 - 0.85 * ramp(close - 0.75, close - 0.25);
                arrive.min(leave)
            }
            "retail" => {
                // staff all day, shoppers building to an after-work peak
                let staff = 0.25 * ramp(open - 0.5, open);
                let shoppers = 0.75
                    * (0.35 + 0.65 * (-((h - 17.5) / 3.0).powi(2)).exp())
                    * ramp(open, open + 0.5);
                (staff + shoppers) * (1.0 - 0.7 * ramp(close - 0.5, close))
            }
            "warehouse" => {
                let shift = ramp(open - 0.25, open + 0.25) * (1.0 - ramp(close - 0.25, close));
                let lunch = if (12.0..12.5).contains(&h) { 0.4 } else { 1.0 };
                shift * lunch
            }
            _ => {
                let arrive = ramp(open - 0.25, open + 1.25);
                let leave = 1.0 - 0.9 * ramp(close - 1.0, close);
                let lunch = 1.0 - 0.3 * (-((h - 12.5) / 0.6).powi(2)).exp();
                arrive.min(leave) * lunch
            }
        }
    }
}
