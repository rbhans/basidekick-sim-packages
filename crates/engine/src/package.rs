//! Building packages: one TOML file describes a whole fake building — its
//! zones, the controllers that serve them, its schedule and the scenarios
//! that make sense for it. The built-in packages live in `packages/` at the
//! repository root and are compiled in; more can be dropped in a folder.

use crate::lesson::LessonDef;
use crate::scenario::ScenarioDef;
use serde::Deserialize;
use std::path::Path;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub id: String,
    pub name: String,
    /// One line for the package picker.
    pub tagline: String,
    #[serde(default)]
    pub description: String,
    /// building | school | store | warehouse — picks the picker icon.
    #[serde(default = "default_icon")]
    pub icon: String,
    pub area_sqft: f64,
    #[serde(default)]
    pub location: String,
    /// Virtual BACnet network the controllers sit on, behind the site router.
    #[serde(default = "default_network")]
    pub network: u16,
    /// Device instance of the site controller; equipment counts up from here.
    #[serde(default = "default_site_instance")]
    pub site_instance: u32,
    /// object-name of the site controller device.
    #[serde(default = "default_site_name")]
    pub site_name: String,
    pub schedule: ScheduleDef,
    #[serde(rename = "zone")]
    pub zones: Vec<ZoneDef>,
    #[serde(rename = "equipment")]
    pub equipment: Vec<EquipDef>,
    #[serde(rename = "scenario", default)]
    pub scenarios: Vec<ScenarioDef>,
    #[serde(rename = "lesson", default)]
    pub lessons: Vec<LessonDef>,
}

fn default_icon() -> String {
    "building".into()
}
fn default_network() -> u16 {
    2001
}
fn default_site_instance() -> u32 {
    10000
}
fn default_site_name() -> String {
    "SITE-CTRL".into()
}

/// Opening hours, as "HH:MM". Days left out are closed.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleDef {
    pub weekday: Option<Hours>,
    #[serde(default)]
    pub saturday: Option<Hours>,
    #[serde(default)]
    pub sunday: Option<Hours>,
    /// HVAC starts this long before people arrive (warm-up / cool-down).
    #[serde(default = "default_lead")]
    pub hvac_lead_min: f64,
    /// How people come and go: office | school | retail | warehouse.
    #[serde(default = "default_profile")]
    pub profile: String,
}

fn default_lead() -> f64 {
    60.0
}
fn default_profile() -> String {
    "office".into()
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(try_from = "[String; 2]")]
pub struct Hours {
    /// Hours after midnight.
    pub open: f64,
    pub close: f64,
}

impl TryFrom<[String; 2]> for Hours {
    type Error = String;
    fn try_from([open, close]: [String; 2]) -> Result<Self, String> {
        Ok(Hours {
            open: parse_hhmm(&open)?,
            close: parse_hhmm(&close)?,
        })
    }
}

fn parse_hhmm(s: &str) -> Result<f64, String> {
    let (h, m) = s
        .split_once(':')
        .ok_or_else(|| format!("expected HH:MM, got {s:?}"))?;
    let h: f64 = h.trim().parse().map_err(|_| format!("bad hour in {s:?}"))?;
    let m: f64 = m
        .trim()
        .parse()
        .map_err(|_| format!("bad minute in {s:?}"))?;
    Ok(h + m / 60.0)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZoneDef {
    pub id: String,
    pub name: String,
    pub area_sqft: f64,
    #[serde(default = "default_height")]
    pub height_ft: f64,
    /// People at full occupancy.
    #[serde(default)]
    pub people: f64,
    #[serde(default = "default_lights")]
    pub lights_w_sqft: f64,
    #[serde(default = "default_plug")]
    pub plug_w_sqft: f64,
    /// 0 for a core zone, 1 for a zone with lots of outside wall and roof.
    #[serde(default = "default_exposure")]
    pub exposure: f64,
    /// Which way its glass faces: north | east | south | west | roof | core.
    #[serde(default = "default_facing")]
    pub facing: String,
    /// Extra outside air when its doors stand open (dock doors, entries).
    #[serde(default)]
    pub door_cfm: f64,
    /// Constant internal heat, BTU/h (servers, kitchen, freezers' condensers…).
    #[serde(default)]
    pub process_btuh: f64,
    /// Starting temperature, °F.
    #[serde(default = "default_start_temp")]
    pub start_temp_f: f64,
    /// CO2 each person breathes out, cfm (adults at a desk ≈ 0.0105,
    /// grade-school kids ≈ 0.0065).
    #[serde(default = "default_co2")]
    pub co2_cfm_per_person: f64,
}

fn default_height() -> f64 {
    10.0
}
fn default_lights() -> f64 {
    0.8
}
fn default_plug() -> f64 {
    0.75
}
fn default_exposure() -> f64 {
    0.5
}
fn default_facing() -> String {
    "south".into()
}
fn default_start_temp() -> f64 {
    71.0
}
fn default_co2() -> f64 {
    0.0105
}

/// One controller, one BACnet device.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EquipDef {
    Rtu(RtuDef),
    Ahu(AhuDef),
    Vav(VavDef),
    ChillerPlant(ChillerDef),
    BoilerPlant(BoilerDef),
    ExhaustFan(FanDef),
    Lighting(LightingDef),
    Meter(MeterDef),
    UnitHeater(UnitHeaterDef),
}

impl EquipDef {
    pub fn common(&self) -> &Common {
        match self {
            EquipDef::Rtu(d) => &d.common,
            EquipDef::Ahu(d) => &d.common,
            EquipDef::Vav(d) => &d.common,
            EquipDef::ChillerPlant(d) => &d.common,
            EquipDef::BoilerPlant(d) => &d.common,
            EquipDef::ExhaustFan(d) => &d.common,
            EquipDef::Lighting(d) => &d.common,
            EquipDef::Meter(d) => &d.common,
            EquipDef::UnitHeater(d) => &d.common,
        }
    }
}

/// Fields every controller has.
#[derive(Clone, Debug, Deserialize)]
pub struct Common {
    /// Device object-name: `RTU-1`, `VAV-104`.
    pub id: String,
    #[serde(default)]
    pub description: String,
    /// Device instance; defaults to the site instance plus its position.
    #[serde(default)]
    pub instance: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cooling {
    #[default]
    Dx,
    Chw,
    None,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Heating {
    #[default]
    Gas,
    Hw,
    Electric,
    None,
}

/// Packaged single-zone rooftop unit.
#[derive(Clone, Debug, Deserialize)]
pub struct RtuDef {
    #[serde(flatten)]
    pub common: Common,
    pub zone: String,
    pub supply_cfm: f64,
    #[serde(default = "default_tons")]
    pub cool_tons: f64,
    #[serde(default = "default_stages")]
    pub cool_stages: u8,
    #[serde(default)]
    pub heating: Heating,
    #[serde(default = "default_heat_mbh")]
    pub heat_mbh: f64,
    #[serde(default = "default_stages")]
    pub heat_stages: u8,
    #[serde(default = "yes")]
    pub economizer: bool,
    #[serde(default = "default_min_oa")]
    pub min_oa_pct: f64,
}

/// VAV air handler serving VAV boxes.
#[derive(Clone, Debug, Deserialize)]
pub struct AhuDef {
    #[serde(flatten)]
    pub common: Common,
    pub supply_cfm: f64,
    #[serde(default)]
    pub cooling: Cooling,
    /// DX only.
    #[serde(default = "default_tons")]
    pub cool_tons: f64,
    #[serde(default)]
    pub heating: Heating,
    /// Gas / electric heat only.
    #[serde(default = "default_heat_mbh")]
    pub heat_mbh: f64,
    #[serde(default)]
    pub chw_plant: Option<String>,
    #[serde(default)]
    pub hw_plant: Option<String>,
    #[serde(default = "yes")]
    pub economizer: bool,
    #[serde(default = "default_min_oa")]
    pub min_oa_pct: f64,
}

/// Pressure-independent VAV box.
#[derive(Clone, Debug, Deserialize)]
pub struct VavDef {
    #[serde(flatten)]
    pub common: Common,
    pub ahu: String,
    pub zone: String,
    pub max_cfm: f64,
    #[serde(default)]
    pub min_cfm: Option<f64>,
    /// hw | electric | none.
    #[serde(default = "default_reheat")]
    pub reheat: Heating,
    /// Electric reheat size.
    #[serde(default = "default_reheat_kw")]
    pub reheat_kw: f64,
    /// Hot water from this plant; defaults to the AHU's.
    #[serde(default)]
    pub hw_plant: Option<String>,
    /// Zone CO2 sensor on the box.
    #[serde(default)]
    pub co2: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChillerDef {
    #[serde(flatten)]
    pub common: Common,
    pub tons: f64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BoilerDef {
    #[serde(flatten)]
    pub common: Common,
    pub mbh: f64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FanDef {
    #[serde(flatten)]
    pub common: Common,
    pub cfm: f64,
    /// Runs whenever this unit's supply fan runs; otherwise on the schedule.
    #[serde(default)]
    pub interlock: Option<String>,
    #[serde(default)]
    pub zone: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct LightingDef {
    #[serde(flatten)]
    pub common: Common,
    /// One relay per zone.
    pub zones: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MeterDef {
    #[serde(flatten)]
    pub common: Common,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UnitHeaterDef {
    #[serde(flatten)]
    pub common: Common,
    pub zone: String,
    pub mbh: f64,
    #[serde(default)]
    pub heating: Heating,
}

fn yes() -> bool {
    true
}
fn default_tons() -> f64 {
    10.0
}
fn default_stages() -> u8 {
    2
}
fn default_heat_mbh() -> f64 {
    150.0
}
fn default_min_oa() -> f64 {
    15.0
}
fn default_reheat() -> Heating {
    Heating::Hw
}
fn default_reheat_kw() -> f64 {
    5.0
}

/// The packages compiled into the app, in picker order.
pub const BUILTIN: &[(&str, &str)] = &[
    (
        "small-office",
        include_str!("../../../packages/small-office.toml"),
    ),
    (
        "elementary-school",
        include_str!("../../../packages/elementary-school.toml"),
    ),
    (
        "retail-store",
        include_str!("../../../packages/retail-store.toml"),
    ),
    (
        "warehouse",
        include_str!("../../../packages/warehouse.toml"),
    ),
];

impl Package {
    pub fn parse(text: &str) -> Result<Package, String> {
        let pkg: Package = toml::from_str(text).map_err(|e| e.to_string())?;
        pkg.validate()?;
        Ok(pkg)
    }

    pub fn load(path: &Path) -> Result<Package, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Package::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn builtin() -> Vec<Package> {
        BUILTIN
            .iter()
            .map(|(id, text)| {
                Package::parse(text).unwrap_or_else(|e| panic!("built-in package {id}: {e}"))
            })
            .collect()
    }

    /// Built-ins plus every `*.toml` in `dir` (a file whose id matches a
    /// built-in replaces it). Bad files come back as errors, not panics.
    pub fn discover(dir: Option<&Path>) -> (Vec<Package>, Vec<String>) {
        let mut packages = Package::builtin();
        let mut errors = Vec::new();
        let Some(dir) = dir else {
            return (packages, errors);
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return (packages, errors);
        };
        let mut paths: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        paths.sort();
        for path in paths {
            match Package::load(&path) {
                Ok(pkg) => match packages.iter_mut().find(|p| p.id == pkg.id) {
                    Some(slot) => *slot = pkg,
                    None => packages.push(pkg),
                },
                Err(e) => errors.push(e),
            }
        }
        (packages, errors)
    }

    pub fn zone_index(&self, id: &str) -> Option<usize> {
        self.zones.iter().position(|z| z.id == id)
    }

    fn validate(&self) -> Result<(), String> {
        let mut ids = std::collections::HashSet::new();
        for e in &self.equipment {
            let c = e.common();
            if !ids.insert(c.id.as_str()) {
                return Err(format!("two pieces of equipment are called {}", c.id));
            }
        }
        if self.equipment.len() > 250 {
            return Err("at most 250 controllers fit on the virtual network".into());
        }
        let zone = |z: &str, who: &str| {
            self.zone_index(z)
                .map(|_| ())
                .ok_or_else(|| format!("{who} serves zone {z:?}, which isn't defined"))
        };
        let equip = |id: &str, who: &str| {
            ids.contains(id)
                .then_some(())
                .ok_or_else(|| format!("{who} refers to {id:?}, which isn't defined"))
        };
        for e in &self.equipment {
            let who = &e.common().id;
            match e {
                EquipDef::Rtu(d) => zone(&d.zone, who)?,
                EquipDef::Ahu(d) => {
                    if let Some(p) = &d.chw_plant {
                        equip(p, who)?;
                    }
                    if let Some(p) = &d.hw_plant {
                        equip(p, who)?;
                    }
                }
                EquipDef::Vav(d) => {
                    zone(&d.zone, who)?;
                    equip(&d.ahu, who)?;
                    if let Some(p) = &d.hw_plant {
                        equip(p, who)?;
                    }
                }
                EquipDef::ExhaustFan(d) => {
                    if let Some(z) = &d.zone {
                        zone(z, who)?;
                    }
                    if let Some(i) = &d.interlock {
                        equip(i, who)?;
                    }
                }
                EquipDef::Lighting(d) => {
                    for z in &d.zones {
                        zone(z, who)?;
                    }
                }
                EquipDef::UnitHeater(d) => zone(&d.zone, who)?,
                EquipDef::ChillerPlant(_) | EquipDef::BoilerPlant(_) | EquipDef::Meter(_) => {}
            }
        }
        for s in &self.scenarios {
            for target in s.effects.iter().filter_map(|e| e.equipment()) {
                if target != "*" {
                    equip(target, &format!("scenario {}", s.id))?;
                }
            }
            for z in s.effects.iter().filter_map(|e| e.zone()) {
                zone(z, &format!("scenario {}", s.id))?;
            }
        }
        let globals = crate::scenario::global_scenarios();
        for l in &self.lessons {
            let who = format!("lesson {}", l.id);
            l.setup.season().map_err(|e| format!("{who}: {e}"))?;
            l.setup.hour().map_err(|e| format!("{who}: {e}"))?;
            if let Some(d) = &l.setup.device {
                equip(d, &who)?;
            }
            for id in &l.setup.scenarios {
                if !self.scenarios.iter().chain(&globals).any(|s| &s.id == id) {
                    return Err(format!("{who} starts scenario {id:?}, which isn't defined"));
                }
            }
        }
        Ok(())
    }
}
