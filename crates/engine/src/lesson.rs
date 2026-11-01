//! Guided lessons: a package can carry short exercises that put the building
//! in the right spot (season, time of day, a scenario or two), say what to
//! do and what the student should see.

use crate::sim::{Level, Simulation};
use crate::world::Season;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LessonDef {
    pub id: String,
    pub title: String,
    /// Beginner, Intermediate or Advanced.
    #[serde(default = "default_level")]
    pub level: String,
    /// What it teaches, in a sentence or two.
    pub goal: String,
    #[serde(default)]
    pub setup: LessonSetup,
    /// What to do, a step each.
    pub steps: Vec<String>,
    /// What they should see.
    pub expect: String,
}

fn default_level() -> String {
    "Beginner".into()
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LessonSetup {
    /// winter, mild or summer.
    pub season: Option<String>,
    /// "HH:MM": the sim runs ahead to it, on a weekday.
    pub time: Option<String>,
    pub speed: Option<f64>,
    /// Scenarios to start once it's there.
    #[serde(default)]
    pub scenarios: Vec<String>,
    /// The controller to open, and which of its charts.
    pub device: Option<String>,
    pub chart: Option<String>,
}

impl LessonSetup {
    pub fn season(&self) -> Result<Option<Season>, String> {
        self.season
            .as_deref()
            .map(|s| match s {
                "winter" => Ok(Season::Winter),
                "mild" => Ok(Season::Mild),
                "summer" => Ok(Season::Summer),
                other => Err(format!("unknown season {other:?}")),
            })
            .transpose()
    }

    /// Hours after midnight.
    pub fn hour(&self) -> Result<Option<f64>, String> {
        self.time
            .as_deref()
            .map(|t| {
                let (h, m) = t
                    .split_once(':')
                    .ok_or_else(|| format!("expected HH:MM, got {t:?}"))?;
                let h: f64 = h.trim().parse().map_err(|_| format!("bad hour in {t:?}"))?;
                let m: f64 = m
                    .trim()
                    .parse()
                    .map_err(|_| format!("bad minute in {t:?}"))?;
                Ok(h + m / 60.0)
            })
            .transpose()
    }
}

impl Simulation {
    /// Puts the building back as built, then where the lesson wants it.
    pub fn start_lesson(&mut self, id: &str) -> Result<(), String> {
        let lesson = self
            .package
            .lessons
            .iter()
            .find(|l| l.id == id)
            .cloned()
            .ok_or_else(|| format!("no lesson {id}"))?;
        let s = &lesson.setup;
        self.stop_all();
        self.clear_overrides();
        if let Some(season) = s.season()? {
            self.set_season(season);
        }
        if let Some(hour) = s.hour()? {
            self.jump_to(hour, true);
        }
        if let Some(speed) = s.speed {
            self.speed = speed;
        }
        for scenario in &s.scenarios {
            self.start_scenario(scenario)?;
        }
        self.note(Level::Info, format!("Lesson: {}", lesson.title));
        Ok(())
    }
}
