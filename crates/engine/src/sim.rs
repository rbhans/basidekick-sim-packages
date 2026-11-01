//! One running building: the clock, the zones, every controller, the active
//! scenarios and the log. The app owns it behind a mutex and calls
//! [`Simulation::advance`] a few times a second.

use crate::equip::*;
use crate::history::{History, Span, Trend};
use crate::package::{EquipDef, Package};
use crate::scenario::*;
use crate::world::{self, Schedule, Season, Weather, hour_of};
use crate::zone::Zone;
use bask_points::{BacObject, PointDb, PointRef, PointValue, SharedDb};
use chrono::{Datelike, Duration, NaiveDateTime, Weekday};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

/// Longest physics step, sim seconds.
const MAX_STEP_S: f64 = 1.0;
/// Points every controller keeps a BACnet trend log of, when it has them.
const TRENDED: [&str; 11] = [
    "ZN-T", "ZN-CO2", "ZN-RH", "SA-T", "DSP", "AIRFLOW", "MA-T", "CHWS-T", "HWS-T", "OA-T", "KW",
];
const LOG_LEN: usize = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Scenario,
    Alarm,
    Clear,
    Bacnet,
}

#[derive(Clone, Debug)]
pub struct LogEntry {
    /// Counts up from 1 over the life of the sim.
    pub seq: u64,
    pub at: NaiveDateTime,
    pub level: Level,
    pub text: String,
}

/// What kind of controller a device is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Site,
    Rtu,
    Ahu,
    Vav,
    Chiller,
    Boiler,
    ExhaustFan,
    Lighting,
    Meter,
    UnitHeater,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Site => "Site controller / router",
            Kind::Rtu => "Rooftop unit",
            Kind::Ahu => "Air handler",
            Kind::Vav => "VAV box",
            Kind::Chiller => "Chiller plant",
            Kind::Boiler => "Boiler plant",
            Kind::ExhaustFan => "Exhaust fan",
            Kind::Lighting => "Lighting panel",
            Kind::Meter => "Electric meter",
            Kind::UnitHeater => "Unit heater",
        }
    }
}

pub struct Simulation {
    pub package: Package,
    pub db: SharedDb,
    pub now: NaiveDateTime,
    pub speed: f64,
    pub paused: bool,
    pub season: Season,
    pub weather: Weather,
    schedule: Schedule,
    pub zones: Vec<Zone>,
    site: Site,
    rtus: Vec<Rtu>,
    ahus: Vec<Ahu>,
    vavs: Vec<Vav>,
    chillers: Vec<ChillerPlant>,
    boilers: Vec<BoilerPlant>,
    fans: Vec<ExhaustFan>,
    lights: Vec<Lighting>,
    meters: Vec<Meter>,
    heaters: Vec<UnitHeater>,
    on_panel: Vec<bool>,
    kinds: Vec<Kind>,
    bus: Bus,
    pub catalog: Vec<ScenarioDef>,
    pub active: Vec<Active>,
    mods: Mods,
    dev_by_id: HashMap<String, usize>,
    pub log: VecDeque<LogEntry>,
    log_base: u64,
    /// Mystery fault id → what it really is.
    mysteries: HashMap<String, String>,
    alarms: HashSet<PointRef>,
    pub energy: Energy,
    pub people: f64,
    pub occupied: bool,
    /// The day optimal start brought the HVAC on, how many minutes before
    /// the schedule did, and when the schedule comes on.
    optimal_start: Option<(chrono::NaiveDate, f64, NaiveDateTime)>,
    /// Consumption when the package has no meter.
    kwh: f64,
    history: History,
    /// Writable settings as the building was built, for putting it back.
    as_built: Vec<(PointRef, PointValue)>,
    as_built_schedule: Option<bask_points::Schedule>,
}

impl Simulation {
    pub fn new(package: Package, start: NaiveDateTime) -> Result<Simulation, String> {
        let mut db = PointDb::new(package.network);
        let zones: Vec<Zone> = package.zones.iter().map(Zone::new).collect();
        let pkg = &package;

        // typed positions, so references can be resolved while building
        let mut typed: HashMap<&str, usize> = HashMap::new();
        let mut counts = [0usize; 3];
        for e in &pkg.equipment {
            let slot = match e {
                EquipDef::Ahu(_) => 0,
                EquipDef::ChillerPlant(_) => 1,
                EquipDef::BoilerPlant(_) => 2,
                _ => continue,
            };
            typed.insert(e.common().id.as_str(), counts[slot]);
            counts[slot] += 1;
        }
        let zone_of = |id: &str| {
            pkg.zone_index(id)
                .ok_or_else(|| format!("unknown zone {id}"))
        };
        let typed_of = |id: &str| {
            typed
                .get(id)
                .copied()
                .ok_or_else(|| format!("unknown equipment {id}"))
        };

        let site = {
            let mut b = DevBuilder::new(
                &mut db,
                pkg.site_instance,
                &pkg.site_name,
                &format!("Site controller and BACnet router, {}", pkg.name),
                "BASidekick SimSite",
                None,
                &pkg.location,
            );
            Site::new(&mut b, &pkg.schedule)
        };
        let mut kinds = vec![Kind::Site];
        let (mut rtus, mut ahus, mut vavs, mut chillers, mut boilers) =
            (vec![], vec![], vec![], vec![], vec![]);
        let (mut fans, mut lights, mut meters, mut heaters) = (vec![], vec![], vec![], vec![]);
        let mut interlocks = Vec::new();
        for (i, e) in pkg.equipment.iter().enumerate() {
            let c = e.common();
            let instance = c.instance.unwrap_or(pkg.site_instance + i as u32 + 1);
            let model = match e {
                EquipDef::Rtu(_) => "BASidekick SimDDC-RTU",
                EquipDef::Ahu(_) => "BASidekick SimDDC-AHU",
                EquipDef::Vav(_) => "BASidekick SimDDC-VAV",
                EquipDef::ChillerPlant(_) | EquipDef::BoilerPlant(_) => "BASidekick SimDDC-PLANT",
                EquipDef::ExhaustFan(_) | EquipDef::UnitHeater(_) => "BASidekick SimDDC-UC",
                EquipDef::Lighting(_) => "BASidekick SimLTG-16",
                EquipDef::Meter(_) => "BASidekick SimMTR",
            };
            let description = if c.description.is_empty() {
                c.id.clone()
            } else {
                c.description.clone()
            };
            let mut b = DevBuilder::new(
                &mut db,
                instance,
                &c.id,
                &description,
                model,
                Some(i as u8 + 1),
                &pkg.location,
            );
            let kind = match e {
                EquipDef::Rtu(d) => {
                    rtus.push(Rtu::new(d, zone_of(&d.zone)?, &mut b));
                    Kind::Rtu
                }
                EquipDef::Ahu(d) => {
                    let chw = d.chw_plant.as_deref().map(typed_of).transpose()?;
                    let hw = d.hw_plant.as_deref().map(typed_of).transpose()?;
                    ahus.push(Ahu::new(d, chw, hw, &mut b));
                    Kind::Ahu
                }
                EquipDef::Vav(d) => {
                    let ahu = typed_of(&d.ahu)?;
                    let hw_id = d.hw_plant.clone().or_else(|| {
                        match pkg.equipment.iter().find(|e| e.common().id == d.ahu) {
                            Some(EquipDef::Ahu(a)) => a.hw_plant.clone(),
                            _ => None,
                        }
                    });
                    let hw = hw_id.as_deref().map(typed_of).transpose()?;
                    vavs.push(Vav::new(d, ahu, zone_of(&d.zone)?, hw, &mut b));
                    Kind::Vav
                }
                EquipDef::ChillerPlant(d) => {
                    chillers.push(ChillerPlant::new(d, &mut b));
                    Kind::Chiller
                }
                EquipDef::BoilerPlant(d) => {
                    boilers.push(BoilerPlant::new(d, &mut b));
                    Kind::Boiler
                }
                EquipDef::ExhaustFan(d) => {
                    interlocks.push(d.interlock.clone());
                    fans.push(ExhaustFan::new(d, None, &mut b));
                    Kind::ExhaustFan
                }
                EquipDef::Lighting(d) => {
                    let served: Vec<(usize, &Zone)> = d
                        .zones
                        .iter()
                        .map(|z| zone_of(z).map(|i| (i, &zones[i])))
                        .collect::<Result<_, _>>()?;
                    lights.push(Lighting::new(&c.id, &served, &mut b));
                    Kind::Lighting
                }
                EquipDef::Meter(_) => {
                    meters.push(Meter::new(&c.id, &mut b));
                    Kind::Meter
                }
                EquipDef::UnitHeater(d) => {
                    heaters.push(UnitHeater::new(d, zone_of(&d.zone)?, &mut b));
                    Kind::UnitHeater
                }
            };
            kinds.push(kind);
        }
        for (fan, interlock) in fans.iter_mut().zip(interlocks) {
            if let Some(id) = interlock {
                let d = db
                    .devices
                    .iter()
                    .position(|d| d.name == id)
                    .ok_or_else(|| format!("unknown interlock {id}"))?;
                let o = db.devices[d]
                    .find_by_name("SF-S")
                    .ok_or_else(|| format!("{id} has no fan status to interlock with"))?;
                fan.interlock = Some(PointRef {
                    device: d as u16,
                    object: o as u16,
                });
            }
        }
        for (i, v) in vavs.iter().enumerate() {
            ahus[v.ahu].vavs.push(i);
        }
        // what a controller keeps on board for a BAS: a trend log of its main
        // readings, and an alarm when a zone sensor fails or reads absurdly
        for dev in db.devices.iter_mut() {
            let mut next = 1;
            let mut logs = Vec::new();
            for o in dev.objects.iter_mut() {
                if o.name == "ZN-T" {
                    o.enable_out_of_range(40.0, 100.0, 1.0, ALARM_CLASS, 60);
                }
                if TRENDED.contains(&o.name.as_str()) {
                    let mut tl = BacObject::trend_log(
                        next,
                        format!("TL-{}", o.name),
                        (o.object_type, o.instance),
                        60,
                        1440,
                    );
                    tl.description = format!("{}: a sample a minute for a day", o.description);
                    logs.push(tl);
                    next += 1;
                }
            }
            dev.objects.extend(logs);
        }

        let mut zones = zones;
        for r in &rtus {
            zones[r.zone].served_by.push(r.id.clone());
        }
        for v in &vavs {
            zones[v.zone]
                .served_by
                .push(format!("{} ← {}", v.id, ahus[v.ahu].id));
        }
        for u in &heaters {
            zones[u.zone].served_by.push(u.id.clone());
        }
        let mut on_panel = vec![false; zones.len()];
        for l in &lights {
            for (z, _) in &l.relays {
                on_panel[*z] = true;
            }
        }

        let dev_by_id = db
            .devices
            .iter()
            .enumerate()
            .map(|(i, d)| (d.name.clone(), i))
            .collect();
        let devices = db.devices.len();
        let mut catalog = global_scenarios();
        catalog.extend(pkg.scenarios.iter().cloned());
        let bus = Bus {
            air: vec![Default::default(); zones.len()],
            chw: vec![Default::default(); chillers.len()],
            hw: vec![Default::default(); boilers.len()],
            chw_demand: vec![Default::default(); chillers.len()],
            hw_demand: vec![Default::default(); boilers.len()],
            energy: Energy::default(),
        };
        let mods = Mods::new(zones.len(), devices);
        let schedule = Schedule::new(package.schedule.clone());
        // rooms start about as damp as the season makes them, within what
        // an HVAC system keeps
        let outside = world::weather(start, Season::for_month(start.month()), 0.0);
        for z in zones.iter_mut() {
            z.w = outside.humidity_ratio().clamp(0.003, 0.0095);
        }
        let history = History::new(&db);
        let mut sim = Simulation {
            db: Arc::new(Mutex::new(db)),
            now: start - Duration::hours(3),
            speed: 1.0,
            paused: false,
            season: Season::for_month(start.month()),
            weather: Weather::default(),
            schedule,
            zones,
            site,
            rtus,
            ahus,
            vavs,
            chillers,
            boilers,
            fans,
            lights,
            meters,
            heaters,
            on_panel,
            kinds,
            bus,
            catalog,
            active: Vec::new(),
            mods,
            dev_by_id,
            log: VecDeque::new(),
            log_base: 0,
            mysteries: HashMap::new(),
            alarms: HashSet::new(),
            energy: Energy::default(),
            people: 0.0,
            occupied: false,
            optimal_start: None,
            kwh: 0.0,
            history,
            as_built: Vec::new(),
            as_built_schedule: None,
            package,
        };
        // settle: run the three hours before the start so loops and loops'
        // water are where they'd be
        sim.run_for(3.0 * 3600.0, 5.0);
        sim.as_built = {
            let mut db = sim.db.lock();
            // nothing from the settling hours goes out on the network
            db.take_notices();
            sim.as_built_schedule = db.obj(sim.site.schedule).as_schedule().cloned();
            db.devices
                .iter()
                .enumerate()
                .flat_map(|(d, dev)| {
                    dev.objects
                        .iter()
                        .enumerate()
                        .filter(|(_, o)| o.writable)
                        .map(move |(o, obj)| {
                            (
                                PointRef {
                                    device: d as u16,
                                    object: o as u16,
                                },
                                obj.present_value,
                            )
                        })
                })
                .collect()
        };
        sim.alarms = sim.current_alarms();
        sim.log.clear();
        sim.note(
            Level::Info,
            format!(
                "Loaded {} · {} controllers on network {}",
                sim.package.name, devices, sim.package.network
            ),
        );
        Ok(sim)
    }

    pub fn kind(&self, device: usize) -> Kind {
        self.kinds[device]
    }

    pub fn device_index(&self, id: &str) -> Option<usize> {
        self.dev_by_id.get(id).copied()
    }

    pub fn note(&mut self, level: Level, text: impl Into<String>) {
        let seq = self.log.back().map_or(self.log_base, |e| e.seq) + 1;
        self.log.push_back(LogEntry {
            seq,
            at: self.now,
            level,
            text: text.into(),
        });
        while self.log.len() > LOG_LEN {
            self.log.pop_front();
        }
    }

    /// Move the sim forward by `real_s` of wall time at the current speed.
    pub fn advance(&mut self, real_s: f64) {
        if self.paused {
            return;
        }
        self.run_for(real_s * self.speed, MAX_STEP_S);
        self.check_alarms();
    }

    fn run_for(&mut self, sim_s: f64, max_step: f64) {
        if sim_s <= 0.0 {
            return;
        }
        let steps = (sim_s / max_step).ceil().max(1.0) as usize;
        let dt = sim_s / steps as f64;
        let db = self.db.clone();
        let mut db = db.lock();
        for _ in 0..steps {
            self.step(&mut db, dt);
        }
    }

    fn step(&mut self, db: &mut PointDb, dt: f64) {
        self.now += Duration::microseconds((dt * 1e6).round() as i64);
        self.tick_scenarios(dt);
        self.mods = self.build_mods(db);
        db.tick(self.now);
        let scheduled = self.scheduled_hvac(db);
        let m = &self.mods;

        self.weather = world::weather(self.now, self.season, m.oat_delta);
        let share = self.schedule.people(self.now);
        let was_occupied = self.occupied;
        self.occupied = self.site.step(db, m, &self.weather, scheduled, self.people);
        let env = Env {
            dt,
            now: self.now,
            weather: self.weather,
            occupied: self.occupied,
            lights: self.schedule.lights_on(self.now) || m.force_occupied,
            mods: m,
        };
        for (i, d) in db.devices.iter_mut().enumerate() {
            d.online = !(m.offline[i] || m.power_out);
        }

        let chw_demand = std::mem::replace(
            &mut self.bus.chw_demand,
            vec![Default::default(); self.chillers.len()],
        );
        let hw_demand = std::mem::replace(
            &mut self.bus.hw_demand,
            vec![Default::default(); self.boilers.len()],
        );
        self.bus
            .air
            .iter_mut()
            .for_each(|a| *a = Default::default());
        self.bus.energy = Energy::default();

        for (i, c) in self.chillers.iter_mut().enumerate() {
            c.step(&env, db, chw_demand[i], &mut self.bus);
            self.bus.chw[i] = c.out;
        }
        for (i, b) in self.boilers.iter_mut().enumerate() {
            b.step(&env, db, hw_demand[i], &mut self.bus);
            self.bus.hw[i] = b.out;
        }
        for a in self.ahus.iter_mut() {
            a.step(&env, db, &self.zones, &self.vavs, &mut self.bus);
        }
        for v in self.vavs.iter_mut() {
            v.step(&env, db, &self.zones, &self.ahus[v.ahu].out, &mut self.bus);
        }
        for r in self.rtus.iter_mut() {
            r.step(&env, db, &self.zones, &mut self.bus);
        }
        for u in self.heaters.iter_mut() {
            u.step(&env, db, &self.zones, &mut self.bus);
        }
        for f in self.fans.iter_mut() {
            f.step(&env, db, &mut self.bus);
        }
        for (z, zone) in self.zones.iter_mut().enumerate() {
            if !self.on_panel[z] {
                zone.lights_on = env.lights && !m.power_out;
            }
        }
        for l in self.lights.iter_mut() {
            l.step(&env, db, &mut self.zones);
        }

        let hour = hour_of(self.now);
        let plugs = if m.power_out {
            0.0
        } else if share > 0.05 || self.occupied {
            0.9
        } else {
            0.35
        };
        let mut people = 0.0;
        for (i, z) in self.zones.iter_mut().enumerate() {
            let here = if m.fire {
                0.0
            } else {
                (share * m.people_factor[i]).max(m.people_floor[i])
            };
            z.step(
                dt,
                hour,
                &self.weather,
                &self.bus.air[i],
                here,
                plugs,
                m.gain_btuh[i],
                m.door_cfm[i],
            );
            people += z.people;
            self.bus.energy.lighting_kw += z.lights_kw();
            self.bus.energy.plug_kw += z.plug_kw(plugs);
        }
        self.people = people;
        for meter in self.meters.iter_mut() {
            meter.step(&env, db, &self.bus.energy);
        }
        if !m.power_out {
            self.kwh += self.bus.energy.total_kw() * dt / 3600.0;
        }
        self.energy = self.bus.energy.clone();
        self.history.sample(db, self.now);

        if self.occupied != was_occupied && !self.log.is_empty() {
            let text = match self.optimal_start {
                Some((day, early, _)) if self.occupied && day == self.now.date() && early > 1.0 => {
                    format!("Building occupied: optimal start, {early:.0} min before opening")
                }
                _ if self.occupied => "Building occupied".to_string(),
                _ => "Building unoccupied".to_string(),
            };
            self.note(Level::Info, text);
        }
    }

    /// The occupancy schedule (the site's BACnet schedule object, so a BAS
    /// can edit it), with optimal start: the HVAC comes on early by about 12
    /// minutes for every degree the worst zone sits outside 70–74 °F (never
    /// less than the package's lead, at most four hours) ahead of the
    /// schedule coming on.
    fn scheduled_hvac(&mut self, db: &PointDb) -> bool {
        let sched = db.obj(self.site.schedule);
        if sched.present_value.as_bool() {
            return true;
        }
        let today = self.now.date();
        let Some(s) = sched.as_schedule() else {
            return false;
        };
        let Some((on, true)) = s.next_change(self.now, Duration::hours(4)) else {
            return false;
        };
        // started early for this, and the schedule hasn't been edited since
        if self
            .optimal_start
            .is_some_and(|(day, _, at)| day == today && at == on)
        {
            return true;
        }
        let behind = self
            .zones
            .iter()
            .filter(|z| z.setpoints.is_some_and(|(clg, _)| clg < 95.0))
            .map(|z| (70.0 - z.temp).max(z.temp - 74.0).max(0.0))
            .fold(0.0, f64::max);
        let lead_min = (self.schedule.base_lead_min() + behind * 12.0).min(240.0);
        let ahead_min = (on - self.now).num_seconds() as f64 / 60.0;
        if ahead_min <= lead_min {
            self.optimal_start = Some((today, ahead_min, on));
            return true;
        }
        false
    }

    fn tick_scenarios(&mut self, dt: f64) {
        let mut ended = Vec::new();
        for a in self.active.iter_mut() {
            a.elapsed_s += dt;
            if let Some(t) = a.ending_s.as_mut() {
                *t += dt;
            } else if a.remaining_s() == Some(0.0) {
                a.ending_s = Some(0.0);
                ended.push(a.def.name.clone());
            }
        }
        self.active.retain(|a| !a.finished());
        for name in ended {
            self.note(Level::Scenario, format!("Scenario over: {name}"));
        }
    }

    fn build_mods(&self, db: &PointDb) -> Mods {
        let mut m = Mods::new(self.zones.len(), db.devices.len());
        for a in &self.active {
            let s = a.scale();
            if s <= 0.0 {
                continue;
            }
            for (e, r) in a.def.effects.iter().zip(&a.resolved) {
                let devices: &[usize] = if let Resolved::Devices(d) = r { d } else { &[] };
                let zones: &[usize] = if let Resolved::Zones(z) = r { z } else { &[] };
                match e {
                    Effect::OutdoorTemp { delta_f } => m.oat_delta += delta_f * s,
                    Effect::People { factor, floor, .. } => {
                        let all: Vec<usize> = (0..self.zones.len()).collect();
                        for &z in if zones.is_empty() { &all } else { zones } {
                            m.people_factor[z] *= 1.0 + (factor - 1.0) * s;
                            m.people_floor[z] = m.people_floor[z].max(floor * s);
                        }
                    }
                    Effect::ForceOccupied => m.force_occupied = true,
                    Effect::HeatGain { btuh, .. } => {
                        zones.iter().for_each(|&z| m.gain_btuh[z] += btuh * s)
                    }
                    Effect::DoorOpen { cfm, .. } => {
                        for &z in zones {
                            m.door_cfm[z] += cfm.unwrap_or(match self.zones[z].door_cfm() {
                                c if c > 0.0 => c,
                                _ => 1500.0,
                            }) * s;
                        }
                    }
                    Effect::Fail { part, .. } => {
                        for &d in devices {
                            match part {
                                Part::Fan => m.fail_fan[d] = true,
                                Part::Cooling => m.fail_cool[d] = true,
                                Part::Heating => m.fail_heat[d] = true,
                            }
                        }
                    }
                    Effect::Stuck {
                        actuator, position, ..
                    } => {
                        for &d in devices {
                            m.stuck.insert((d, *actuator), *position);
                        }
                    }
                    Effect::Sensor { mode, value, .. } => {
                        if let Resolved::Point(Some(p)) = r {
                            let fault = match mode {
                                SensorMode::Frozen => SensorFault::Frozen,
                                SensorMode::Offset => SensorFault::Offset(value.unwrap_or(5.0) * s),
                                SensorMode::Fail => SensorFault::Fail(value.unwrap_or(-40.0)),
                            };
                            m.sensors.insert(*p, fault);
                        }
                    }
                    Effect::Filter { dp, .. } => {
                        devices.iter().for_each(|&d| m.filter_dp[d] += dp * s)
                    }
                    Effect::Offline { .. } => devices.iter().for_each(|&d| m.offline[d] = true),
                    Effect::PowerOutage => m.power_out = true,
                    Effect::FireAlarm => m.fire = true,
                }
            }
        }
        m
    }

    fn resolve(&self, db: &PointDb, e: &Effect) -> Resolved {
        let devices = |id: &str| -> Vec<usize> {
            if id == "*" {
                (0..db.devices.len()).collect()
            } else {
                self.device_index(id).into_iter().collect()
            }
        };
        let zones = |ids: &[&str]| -> Vec<usize> {
            ids.iter()
                .filter_map(|z| self.package.zone_index(z))
                .collect()
        };
        match e {
            Effect::Fail { equip, .. }
            | Effect::Stuck { equip, .. }
            | Effect::Filter { equip, .. }
            | Effect::Offline { equip } => Resolved::Devices(devices(equip)),
            Effect::Sensor { equip, point, .. } => {
                Resolved::Point(self.device_index(equip).and_then(|d| {
                    db.devices[d].find_by_name(point).map(|o| PointRef {
                        device: d as u16,
                        object: o as u16,
                    })
                }))
            }
            Effect::HeatGain { zone, .. } | Effect::DoorOpen { zone, .. } => {
                Resolved::Zones(zones(&[zone.as_str()]))
            }
            Effect::People { zones: z, .. } => {
                Resolved::Zones(zones(&z.iter().map(String::as_str).collect::<Vec<_>>()))
            }
            _ => Resolved::Nothing,
        }
    }

    /// Starts a scenario from the catalog. Starting one that's running
    /// restarts it.
    pub fn start_scenario(&mut self, id: &str) -> Result<(), String> {
        let def = self
            .catalog
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or_else(|| format!("no scenario {id}"))?;
        self.start(def);
        Ok(())
    }

    /// Starts any scenario, catalog or not (quick faults use this).
    pub fn start(&mut self, def: ScenarioDef) {
        self.active.retain(|a| a.def.id != def.id);
        let resolved = {
            let db = self.db.lock();
            def.effects.iter().map(|e| self.resolve(&db, e)).collect()
        };
        self.note(Level::Scenario, format!("Scenario started: {}", def.name));
        self.active.push(Active {
            def,
            elapsed_s: 0.0,
            ending_s: None,
            resolved,
        });
    }

    /// Stops a scenario; its effects fade out over its ramp.
    pub fn stop_scenario(&mut self, id: &str) {
        let mut stopped = None;
        for a in self
            .active
            .iter_mut()
            .filter(|a| a.def.id == id && a.ending_s.is_none())
        {
            a.ending_s = Some(0.0);
            stopped = Some(a.def.name.clone());
        }
        self.active.retain(|a| !a.finished());
        if let Some(name) = stopped {
            self.note(Level::Scenario, format!("Scenario stopped: {name}"));
        }
    }

    pub fn stop_all(&mut self) {
        let ids: Vec<String> = self
            .active
            .iter()
            .filter(|a| a.ending_s.is_none())
            .map(|a| a.def.id.clone())
            .collect();
        for id in ids {
            self.stop_scenario(&id);
        }
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.active
            .iter()
            .any(|a| a.def.id == id && a.ending_s.is_none())
    }

    pub fn set_season(&mut self, season: Season) {
        if season != self.season {
            self.season = season;
            self.note(Level::Info, format!("Weather set to {}", season.label()));
        }
    }

    /// Runs the sim ahead to the next `hour` (on a weekday if asked), up to
    /// three days, so the building gets there the way it would have.
    pub fn jump_to(&mut self, hour: f64, weekday: bool) {
        let mut target =
            self.now.date().and_hms_opt(0, 0, 0).unwrap() + Duration::minutes((hour * 60.0) as i64);
        while target <= self.now
            || (weekday && matches!(target.weekday(), Weekday::Sat | Weekday::Sun))
        {
            target += Duration::days(1);
        }
        let ahead = (target - self.now).num_seconds() as f64;
        if ahead > 3.0 * 86_400.0 {
            return;
        }
        self.run_for(ahead, 5.0);
        self.check_alarms();
        self.note(
            Level::Info,
            format!("Jumped ahead to {}", target.format("%a %-I:%M %p")),
        );
    }

    fn current_alarms(&self) -> HashSet<PointRef> {
        let db = self.db.lock();
        let mut set = HashSet::new();
        for (d, dev) in db.devices.iter().enumerate() {
            for (o, obj) in dev.objects.iter().enumerate() {
                if obj.in_alarm || obj.fault() {
                    set.insert(PointRef {
                        device: d as u16,
                        object: o as u16,
                    });
                }
            }
        }
        set
    }

    fn check_alarms(&mut self) {
        let now = self.current_alarms();
        let mut notes = Vec::new();
        {
            let db = self.db.lock();
            let name = |r: &PointRef| {
                let o = db.obj(*r);
                format!(
                    "{} {} · {}",
                    db.devices[r.device as usize].name, o.name, o.description
                )
            };
            for r in now.difference(&self.alarms) {
                notes.push((
                    Level::Alarm,
                    format!(
                        "{} {}",
                        if db.obj(*r).fault() {
                            "Fault:"
                        } else {
                            "Alarm:"
                        },
                        name(r)
                    ),
                ));
            }
            for r in self.alarms.difference(&now) {
                notes.push((Level::Clear, format!("Back to normal: {}", name(r))));
            }
        }
        for (level, text) in notes {
            self.note(level, text);
        }
        self.alarms = now;
    }

    pub fn kwh(&self) -> f64 {
        self.meters.first().map(|m| m.total_kwh).unwrap_or(self.kwh)
    }

    pub fn headline(&self, device: usize) -> String {
        let find = |dev: usize| dev == device;
        if device == self.site.dev {
            return format!(
                "{} · OA {}",
                if self.occupied {
                    "Occupied"
                } else {
                    "Unoccupied"
                },
                fmt_f(self.weather.oat)
            );
        }
        self.rtus
            .iter()
            .find(|x| find(x.dev))
            .map(|x| x.headline())
            .or_else(|| self.ahus.iter().find(|x| find(x.dev)).map(|x| x.headline()))
            .or_else(|| self.vavs.iter().find(|x| find(x.dev)).map(|x| x.headline()))
            .or_else(|| {
                self.chillers
                    .iter()
                    .find(|x| find(x.dev))
                    .map(|x| x.headline())
            })
            .or_else(|| {
                self.boilers
                    .iter()
                    .find(|x| find(x.dev))
                    .map(|x| x.headline())
            })
            .or_else(|| self.fans.iter().find(|x| find(x.dev)).map(|x| x.headline()))
            .or_else(|| {
                self.lights
                    .iter()
                    .find(|x| find(x.dev))
                    .map(|x| x.headline())
            })
            .or_else(|| {
                self.meters
                    .iter()
                    .find(|x| find(x.dev))
                    .map(|x| x.headline())
            })
            .or_else(|| {
                self.heaters
                    .iter()
                    .find(|x| find(x.dev))
                    .map(|x| x.headline())
            })
            .unwrap_or_default()
    }

    /// Puts the building back as built: every BAS command released, every
    /// point back in service, every setting at its default.
    pub fn clear_overrides(&mut self) {
        let mut db = self.db.lock();
        for dev in db.devices.iter_mut() {
            for o in dev.objects.iter_mut() {
                o.priority_array = [None; 16];
                o.out_of_service = false;
                o.present_value = o.effective();
            }
        }
        for (r, v) in &self.as_built {
            db.obj_mut(*r).present_value = *v;
        }
        if let (Some(built), Some(s)) = (
            &self.as_built_schedule,
            db.obj_mut(self.site.schedule).as_schedule_mut(),
        ) {
            *s = built.clone();
        }
        let now = self.now;
        db.obj_mut(self.site.schedule).refresh_schedule(now);
    }

    /// A write from the app itself, the way a BAS operator would make it:
    /// commandable points at priority 8, settings directly, and sensors by
    /// taking them out of service. `None` releases.
    pub fn command(
        &mut self,
        device: usize,
        point: &str,
        value: Option<PointValue>,
    ) -> Result<(), String> {
        let text = {
            let mut db = self.db.lock();
            let dev = db.devices.get_mut(device).ok_or("no such controller")?;
            let o = dev.find_by_name(point).ok_or("no such point")?;
            let dev_name = dev.name.clone();
            let obj = &mut dev.objects[o];
            let refused = |e| format!("{point} refused it: {e:?}");
            let what = if obj.commandable {
                obj.write_present_value(value, Some(8)).map_err(refused)?;
                match value {
                    Some(v) => format!("{} @ priority 8", bask_points::format_value(obj, v)),
                    None => "released priority 8".into(),
                }
            } else if obj.object_type.is_input() {
                match value {
                    Some(v) => {
                        let v = obj.coerce(v).map_err(refused)?;
                        obj.out_of_service = true;
                        obj.present_value = v;
                        format!("{}, out of service", bask_points::format_value(obj, v))
                    }
                    None => {
                        obj.out_of_service = false;
                        "back in service".into()
                    }
                }
            } else if obj.writable {
                let v = value.ok_or(format!("{point} is a setting: give it a value"))?;
                obj.write_present_value(Some(v), None).map_err(refused)?;
                bask_points::format_value(obj, obj.present_value)
            } else {
                return Err(format!("{point} is read-only"));
            };
            format!("You set {dev_name} {point}: {what}")
        };
        self.note(Level::Bacnet, text);
        Ok(())
    }

    /// Ready-made faults for one controller, as scenarios to start.
    pub fn quick_faults(&self, device: usize) -> Vec<ScenarioDef> {
        let db = self.db.lock();
        let Some(dev) = db.devices.get(device) else {
            return Vec::new();
        };
        let id = dev.name.clone();
        let has = |name: &str| dev.find_by_name(name).is_some();
        let mut out = Vec::new();
        let mut add =
            |key: &str, name: String, description: &str, ramp_min: f64, effects: Vec<Effect>| {
                out.push(ScenarioDef {
                    id: format!("quick:{id}:{key}"),
                    name,
                    description: description.into(),
                    category: Category::Equipment,
                    duration_min: None,
                    ramp_min,
                    effects,
                });
            };
        let fail = |part| {
            vec![Effect::Fail {
                equip: id.clone(),
                part,
            }]
        };
        match self.kinds[device] {
            Kind::Rtu | Kind::Ahu => {
                add(
                    "fan",
                    format!("{id} supply fan fails"),
                    "Belt or drive failure: commanded on, no status.",
                    0.0,
                    fail(Part::Fan),
                );
                add(
                    "cool",
                    format!("{id} loses cooling"),
                    "Compressors lock out or the coil gets no chilled water.",
                    0.0,
                    fail(Part::Cooling),
                );
                if has("HTG1-C") || has("HTG-VLV") {
                    add(
                        "heat",
                        format!("{id} loses heat"),
                        "Burner lockout or no hot water at the coil.",
                        0.0,
                        fail(Part::Heating),
                    );
                }
                add(
                    "oad",
                    format!("{id} OA damper stuck open"),
                    "Linkage slips; the damper sits at 100 % whatever the command.",
                    0.0,
                    vec![Effect::Stuck {
                        equip: id.clone(),
                        actuator: Actuator::OaDamper,
                        position: 100.0,
                    }],
                );
                add(
                    "sat",
                    format!("{id} supply air sensor drifts"),
                    "Reads 6 °F high, drifting in over an hour.",
                    60.0,
                    vec![Effect::Sensor {
                        equip: id.clone(),
                        point: "SA-T".into(),
                        mode: SensorMode::Offset,
                        value: Some(6.0),
                    }],
                );
                add(
                    "filter",
                    format!("{id} filter loads up"),
                    "Filter pressure drop climbs over two hours.",
                    120.0,
                    vec![Effect::Filter {
                        equip: id.clone(),
                        dp: 0.9,
                    }],
                );
            }
            Kind::Vav => {
                add(
                    "damper",
                    format!("{id} damper stuck at 15 %"),
                    "Actuator fails; airflow falls short and the zone drifts.",
                    0.0,
                    vec![Effect::Stuck {
                        equip: id.clone(),
                        actuator: Actuator::Damper,
                        position: 15.0,
                    }],
                );
                add(
                    "heat",
                    format!("{id} reheat fails"),
                    "No heat from the reheat coil.",
                    0.0,
                    fail(Part::Heating),
                );
            }
            Kind::Chiller => {
                add(
                    "cool",
                    format!("{id} chiller trips"),
                    "Low refrigerant pressure trip. CHW supply climbs.",
                    0.0,
                    fail(Part::Cooling),
                );
                add(
                    "pump",
                    format!("{id} pump fails"),
                    "Pump commanded on, no status, no flow.",
                    0.0,
                    fail(Part::Fan),
                );
            }
            Kind::Boiler => {
                add(
                    "heat",
                    format!("{id} boiler locks out"),
                    "Flame failure lockout. HW supply sags.",
                    0.0,
                    fail(Part::Heating),
                );
                add(
                    "pump",
                    format!("{id} pump fails"),
                    "Pump commanded on, no status, no flow.",
                    0.0,
                    fail(Part::Fan),
                );
            }
            Kind::ExhaustFan => add(
                "fan",
                format!("{id} fails"),
                "Commanded on, no status.",
                0.0,
                fail(Part::Fan),
            ),
            Kind::UnitHeater => {
                add(
                    "fan",
                    format!("{id} fan fails"),
                    "Commanded on, no status, no heat.",
                    0.0,
                    fail(Part::Fan),
                );
                add(
                    "heat",
                    format!("{id} burner fails"),
                    "Fan runs, no flame.",
                    0.0,
                    fail(Part::Heating),
                );
            }
            Kind::Site | Kind::Lighting | Kind::Meter => {}
        }
        if has("ZN-T") {
            add(
                "zn-t",
                format!("{id} zone sensor fails"),
                "Open circuit: reads -40 °F with a fault, and the controller chases it.",
                0.0,
                vec![Effect::Sensor {
                    equip: id.clone(),
                    point: "ZN-T".into(),
                    mode: SensorMode::Fail,
                    value: None,
                }],
            );
            add(
                "zn-t-freeze",
                format!("{id} zone sensor freezes"),
                "Stops updating. No fault flag; only the trend shows it.",
                0.0,
                vec![Effect::Sensor {
                    equip: id.clone(),
                    point: "ZN-T".into(),
                    mode: SensorMode::Frozen,
                    value: None,
                }],
            );
        }
        let off = if self.kinds[device] == Kind::Site {
            "The router goes dark too, so every controller behind it disappears."
        } else {
            "Stops answering BACnet; keeps running on its own logic."
        };
        out.push(ScenarioDef {
            id: format!("quick:{id}:offline"),
            name: format!("{id} drops offline"),
            description: off.into(),
            category: Category::Network,
            duration_min: None,
            ramp_min: 0.0,
            effects: vec![Effect::Offline { equip: id.clone() }],
        });
        out
    }

    /// Breaks something at random, from every controller's quick faults,
    /// under a name that doesn't give it away. Returns its id.
    pub fn start_mystery(&mut self) -> String {
        let choices: Vec<ScenarioDef> = (0..self.kinds.len())
            .filter(|&d| self.kinds[d] != Kind::Site)
            .flat_map(|d| self.quick_faults(d))
            .collect();
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos() as usize);
        let pick = choices[(seed ^ (self.mysteries.len() * 7919)) % choices.len()].clone();
        let n = self.mysteries.len() + 1;
        let id = format!("mystery:{n}");
        self.mysteries.insert(id.clone(), pick.name.clone());
        self.start(ScenarioDef {
            id: id.clone(),
            name: format!("Mystery fault {n}"),
            description: "Something in the building has gone wrong. Find it from the BAS: alarms, trends, commands and statuses that disagree.".into(),
            category: Category::Equipment,
            duration_min: None,
            ramp_min: pick.ramp_min,
            effects: pick.effects,
        });
        id
    }

    /// What a mystery fault really is.
    pub fn mystery_answer(&self, id: &str) -> Option<&str> {
        self.mysteries.get(id).map(String::as_str)
    }

    /// The titles of a controller's trend charts.
    pub fn chart_titles(&self, device: usize) -> Vec<&'static str> {
        self.history.titles(device)
    }

    /// One of a controller's trend charts, the last `span` of it.
    pub fn trend(&self, device: usize, chart: usize, span: Span) -> Option<Trend> {
        self.history.trend(device, chart, span, self.now)
    }

    /// When the BACnet occupancy schedule first comes on today and goes off
    /// again, exceptions included.
    pub fn schedule_today(&self) -> Option<(f64, f64)> {
        let db = self.db.lock();
        let s = db.obj(self.site.schedule).as_schedule()?;
        let midnight = self.now.date().and_hms_opt(0, 0, 0)?;
        let hour = |t: NaiveDateTime| (t - midnight).num_seconds() as f64 / 3600.0;
        let on = if s.value_at(midnight) {
            midnight
        } else {
            match s.next_change(midnight, Duration::days(1))? {
                (t, true) => t,
                _ => return None,
            }
        };
        let off = match s.next_change(on, Duration::days(1)) {
            Some((t, false)) => hour(t).min(24.0),
            _ => 24.0,
        };
        Some((hour(on), off))
    }

    pub fn schedule_hours(&self) -> Option<(f64, f64)> {
        self.schedule.hours(self.now).map(|h| (h.open, h.close))
    }
}
