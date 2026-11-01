//! The BACnet object database a simulated building exposes.
//!
//! Every simulated controller is a [`BacDevice`] and every point on it a
//! [`BacObject`]. The simulation engine writes sensor readings and its own
//! control outputs here; the BACnet/IP server reads and writes the same
//! objects for whatever BAS is talking to it. Both share one [`SharedDb`].
//!
//! How values flow:
//! - Inputs (AI, BI, MSI) carry what the sensor measures. A BACnet client can
//!   only write their present value while the object is out of service, and
//!   then the simulation's control logic sees the written value instead.
//! - Commandable objects (AO, BO, MSO, and AV/BV/MSV marked commandable) carry
//!   a priority array. The simulation's own logic writes the relinquish
//!   default; a BAS write at any priority overrides it until relinquished.
//! - Other values (status AVs and the like) are plain: the simulation writes
//!   the present value, and a client may write them only if `writable` is set.
//!
//! Time is the simulation's. The engine calls [`PointDb::tick`] every step
//! with the building's local time: schedules follow it, trend logs sample on
//! it, and intrinsic reporting times its delays and stamps its events with
//! it. The devices report it as their local date and time too.
//!
//! - Schedules ([`Schedule`]) set their own present value from the weekly
//!   schedule and its exceptions, unless out of service.
//! - Trend logs ([`TrendLog`]) sample another object in the same device.
//! - Points with intrinsic reporting ([`EventReporting`]) run their event
//!   algorithm and queue [`EventNotice`]s, which the BACnet server sends to
//!   the recipients of their [`NotificationClass`]. For them `in_alarm`
//!   follows the event state; for the rest the engine sets it.

mod event;
mod schedule;
mod trend;

pub use event::{
    Destination, EventAlgorithm, EventNotice, EventReporting, EventState, EventType, EventValues,
    NotificationClass, NotifyType, Recipient, Transition,
};
pub use schedule::{
    CalendarEntry, DatePattern, DateRange, Schedule, SpecialEvent, TimeValue, WeekNDay,
};
pub use trend::{LogDatum, LogRecord, LogStatus, TrendLog};

use chrono::{NaiveDateTime, NaiveTime, TimeDelta};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;

pub type SharedDb = Arc<Mutex<PointDb>>;

/// The BACnet object types the simulator serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjectType {
    AnalogInput,
    AnalogOutput,
    AnalogValue,
    BinaryInput,
    BinaryOutput,
    BinaryValue,
    MultiStateInput,
    MultiStateOutput,
    MultiStateValue,
    NotificationClass,
    Schedule,
    TrendLog,
}

impl ObjectType {
    pub const ALL: [ObjectType; 12] = [
        ObjectType::AnalogInput,
        ObjectType::AnalogOutput,
        ObjectType::AnalogValue,
        ObjectType::BinaryInput,
        ObjectType::BinaryOutput,
        ObjectType::BinaryValue,
        ObjectType::MultiStateInput,
        ObjectType::MultiStateOutput,
        ObjectType::MultiStateValue,
        ObjectType::NotificationClass,
        ObjectType::Schedule,
        ObjectType::TrendLog,
    ];

    /// BACnetObjectType enumeration value.
    pub fn bacnet_id(self) -> u16 {
        match self {
            ObjectType::AnalogInput => 0,
            ObjectType::AnalogOutput => 1,
            ObjectType::AnalogValue => 2,
            ObjectType::BinaryInput => 3,
            ObjectType::BinaryOutput => 4,
            ObjectType::BinaryValue => 5,
            ObjectType::MultiStateInput => 13,
            ObjectType::MultiStateOutput => 14,
            ObjectType::MultiStateValue => 19,
            ObjectType::NotificationClass => 15,
            ObjectType::Schedule => 17,
            ObjectType::TrendLog => 20,
        }
    }

    pub fn from_bacnet_id(id: u16) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.bacnet_id() == id)
    }

    pub fn abbrev(self) -> &'static str {
        match self {
            ObjectType::AnalogInput => "AI",
            ObjectType::AnalogOutput => "AO",
            ObjectType::AnalogValue => "AV",
            ObjectType::BinaryInput => "BI",
            ObjectType::BinaryOutput => "BO",
            ObjectType::BinaryValue => "BV",
            ObjectType::MultiStateInput => "MSI",
            ObjectType::MultiStateOutput => "MSO",
            ObjectType::MultiStateValue => "MSV",
            ObjectType::NotificationClass => "NC",
            ObjectType::Schedule => "SCH",
            ObjectType::TrendLog => "TL",
        }
    }

    pub fn is_analog(self) -> bool {
        matches!(
            self,
            ObjectType::AnalogInput | ObjectType::AnalogOutput | ObjectType::AnalogValue
        )
    }

    pub fn is_binary(self) -> bool {
        matches!(
            self,
            ObjectType::BinaryInput | ObjectType::BinaryOutput | ObjectType::BinaryValue
        )
    }

    pub fn is_multistate(self) -> bool {
        matches!(
            self,
            ObjectType::MultiStateInput
                | ObjectType::MultiStateOutput
                | ObjectType::MultiStateValue
        )
    }

    /// Inputs: present value writable only while out of service.
    pub fn is_input(self) -> bool {
        matches!(
            self,
            ObjectType::AnalogInput | ObjectType::BinaryInput | ObjectType::MultiStateInput
        )
    }

    /// Outputs always carry a priority array.
    pub fn is_output(self) -> bool {
        matches!(
            self,
            ObjectType::AnalogOutput | ObjectType::BinaryOutput | ObjectType::MultiStateOutput
        )
    }

    /// Trend logs and notification classes have no present value.
    pub fn has_present_value(self) -> bool {
        !matches!(self, ObjectType::TrendLog | ObjectType::NotificationClass)
    }
}

/// A present value. Multi-state values are 1-based, as in BACnet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointValue {
    Real(f32),
    Binary(bool),
    Multi(u32),
}

impl PointValue {
    pub fn as_f64(self) -> f64 {
        match self {
            PointValue::Real(v) => v as f64,
            PointValue::Binary(b) => b as u8 as f64,
            PointValue::Multi(m) => m as f64,
        }
    }

    pub fn as_bool(self) -> bool {
        match self {
            PointValue::Real(v) => v != 0.0,
            PointValue::Binary(b) => b,
            PointValue::Multi(m) => m > 1,
        }
    }

    pub fn as_u32(self) -> u32 {
        match self {
            PointValue::Real(v) => v.max(0.0).round() as u32,
            PointValue::Binary(b) => b as u32,
            PointValue::Multi(m) => m,
        }
    }
}

/// Engineering units, with their BACnetEngineeringUnits numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Units {
    NoUnits,
    DegreesFahrenheit,
    DeltaDegreesFahrenheit,
    Percent,
    PercentRelativeHumidity,
    PartsPerMillion,
    CubicFeetPerMinute,
    InchesOfWater,
    PoundsPerSquareInch,
    UsGallonsPerMinute,
    Kilowatts,
    KilowattHours,
    KiloBtusPerHour,
    Therms,
    TonsRefrigeration,
    Hours,
    Minutes,
    Seconds,
}

impl Units {
    pub fn bacnet_id(self) -> u32 {
        match self {
            Units::NoUnits => 95,
            Units::DegreesFahrenheit => 64,
            Units::DeltaDegreesFahrenheit => 120,
            Units::Percent => 98,
            Units::PercentRelativeHumidity => 29,
            Units::PartsPerMillion => 96,
            Units::CubicFeetPerMinute => 84,
            Units::InchesOfWater => 58,
            Units::PoundsPerSquareInch => 56,
            Units::UsGallonsPerMinute => 89,
            Units::Kilowatts => 48,
            Units::KilowattHours => 19,
            Units::KiloBtusPerHour => 157,
            Units::Therms => 21,
            Units::TonsRefrigeration => 52,
            Units::Hours => 71,
            Units::Minutes => 72,
            Units::Seconds => 73,
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Units::NoUnits => "",
            Units::DegreesFahrenheit => "°F",
            Units::DeltaDegreesFahrenheit => "Δ°F",
            Units::Percent => "%",
            Units::PercentRelativeHumidity => "%RH",
            Units::PartsPerMillion => "ppm",
            Units::CubicFeetPerMinute => "cfm",
            Units::InchesOfWater => "in. wc",
            Units::PoundsPerSquareInch => "psi",
            Units::UsGallonsPerMinute => "gpm",
            Units::Kilowatts => "kW",
            Units::KilowattHours => "kWh",
            Units::KiloBtusPerHour => "MBH",
            Units::Therms => "thm",
            Units::TonsRefrigeration => "tons",
            Units::Hours => "h",
            Units::Minutes => "min",
            Units::Seconds => "s",
        }
    }

    /// Decimal places worth showing.
    pub fn decimals(self) -> usize {
        match self {
            Units::InchesOfWater => 2,
            Units::Seconds => 0,
            Units::PartsPerMillion
            | Units::CubicFeetPerMinute
            | Units::KilowattHours
            | Units::UsGallonsPerMinute => 0,
            Units::Percent | Units::PercentRelativeHumidity => 0,
            _ => 1,
        }
    }
}

/// BACnetReliability values the simulator uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reliability {
    NoFaultDetected,
    OverRange,
    UnderRange,
    OpenLoop,
    ShortedLoop,
    UnreliableOther,
}

impl Reliability {
    pub fn bacnet_id(self) -> u32 {
        match self {
            Reliability::NoFaultDetected => 0,
            Reliability::OverRange => 2,
            Reliability::UnderRange => 3,
            Reliability::OpenLoop => 4,
            Reliability::ShortedLoop => 5,
            Reliability::UnreliableOther => 7,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Reliability::NoFaultDetected => "no fault",
            Reliability::OverRange => "over range",
            Reliability::UnderRange => "under range",
            Reliability::OpenLoop => "open loop",
            Reliability::ShortedLoop => "shorted loop",
            Reliability::UnreliableOther => "unreliable",
        }
    }
}

/// Why a BACnet write was refused, mapped to a BACnet error by the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteError {
    WriteAccessDenied,
    InvalidDataType,
    ValueOutOfRange,
}

#[derive(Clone, Debug)]
pub struct BacObject {
    pub object_type: ObjectType,
    pub instance: u32,
    /// object-name, unique within the device: `ZN-T`, `SF-C`, …
    pub name: String,
    pub description: String,
    /// Analog objects only.
    pub units: Units,
    pub present_value: PointValue,
    pub commandable: bool,
    pub priority_array: [Option<PointValue>; 16],
    pub relinquish_default: PointValue,
    pub out_of_service: bool,
    pub reliability: Reliability,
    /// status-flags IN_ALARM.
    pub in_alarm: bool,
    /// Multi-state: state-text, one per state. Binary: `[inactive-text, active-text]`.
    pub states: Vec<String>,
    pub cov_increment: f32,
    /// Analog writes outside this range are refused with value-out-of-range.
    pub min_value: Option<f32>,
    pub max_value: Option<f32>,
    /// A non-commandable value object that clients may still write.
    pub writable: bool,
    /// What a schedule, trend log or notification class holds beyond that.
    pub data: ObjectData,
    /// Intrinsic reporting, for points that have it turned on.
    pub reporting: Option<Box<EventReporting>>,
}

/// The part of an object that only some types have.
#[derive(Clone, Debug, Default)]
pub enum ObjectData {
    #[default]
    Point,
    Schedule(Box<Schedule>),
    TrendLog(Box<TrendLog>),
    NotificationClass(Box<NotificationClass>),
}

impl BacObject {
    pub fn new(
        object_type: ObjectType,
        instance: u32,
        name: impl Into<String>,
        initial: PointValue,
    ) -> Self {
        BacObject {
            object_type,
            instance,
            name: name.into(),
            description: String::new(),
            units: Units::NoUnits,
            present_value: initial,
            commandable: object_type.is_output(),
            priority_array: [None; 16],
            relinquish_default: initial,
            out_of_service: false,
            reliability: Reliability::NoFaultDetected,
            in_alarm: false,
            states: if object_type.is_binary() {
                vec!["Off".into(), "On".into()]
            } else {
                Vec::new()
            },
            cov_increment: 0.1,
            min_value: None,
            max_value: None,
            writable: false,
            data: ObjectData::Point,
            reporting: None,
        }
    }

    /// A binary occupancy schedule. `weekly[0]` is Monday; Schedule_Default is
    /// inactive. Its present value follows the schedule at each tick.
    pub fn schedule(
        instance: u32,
        name: impl Into<String>,
        weekly: [Vec<(NaiveTime, bool)>; 7],
    ) -> Self {
        let mut o = BacObject::new(
            ObjectType::Schedule,
            instance,
            name,
            PointValue::Binary(false),
        );
        o.states = vec!["Off".into(), "On".into()];
        o.data = ObjectData::Schedule(Box::new(Schedule::new(weekly)));
        o
    }

    /// A trend log polling the present value of `target`, an object in the
    /// same device, every `interval_s` seconds of sim time, keeping the last
    /// `buffer_size` records.
    pub fn trend_log(
        instance: u32,
        name: impl Into<String>,
        target: (ObjectType, u32),
        interval_s: u32,
        buffer_size: u32,
    ) -> Self {
        let mut o = BacObject::new(ObjectType::TrendLog, instance, name, PointValue::Real(0.0));
        o.data = ObjectData::TrendLog(Box::new(TrendLog::new(target, interval_s, buffer_size)));
        o
    }

    /// A notification class; its number is its instance. Out of the box it
    /// broadcasts every transition, unconfirmed, on the local network.
    pub fn notification_class(instance: u32, name: impl Into<String>) -> Self {
        let mut o = BacObject::new(
            ObjectType::NotificationClass,
            instance,
            name,
            PointValue::Real(0.0),
        );
        o.data = ObjectData::NotificationClass(Box::default());
        o
    }

    /// Intrinsic CHANGE_OF_STATE reporting, for binary inputs and values:
    /// off-normal while the present value is `alarm_value` for
    /// `time_delay_s` seconds of sim time.
    pub fn enable_change_of_state(
        &mut self,
        alarm_value: bool,
        notification_class: u32,
        time_delay_s: u32,
    ) {
        self.reporting = Some(Box::new(EventReporting::new(
            EventAlgorithm::ChangeOfState { alarm_value },
            notification_class,
            time_delay_s,
        )));
    }

    /// Intrinsic OUT_OF_RANGE reporting, for analog inputs and values: high
    /// or low limit once outside `low`..`high` for `time_delay_s` seconds of
    /// sim time, normal again once back inside by `deadband`.
    pub fn enable_out_of_range(
        &mut self,
        low: f32,
        high: f32,
        deadband: f32,
        notification_class: u32,
        time_delay_s: u32,
    ) {
        self.reporting = Some(Box::new(EventReporting::new(
            EventAlgorithm::OutOfRange {
                low,
                high,
                deadband,
                low_enable: true,
                high_enable: true,
            },
            notification_class,
            time_delay_s,
        )));
    }

    pub fn as_schedule(&self) -> Option<&Schedule> {
        match &self.data {
            ObjectData::Schedule(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_schedule_mut(&mut self) -> Option<&mut Schedule> {
        match &mut self.data {
            ObjectData::Schedule(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_trend_log(&self) -> Option<&TrendLog> {
        match &self.data {
            ObjectData::TrendLog(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_trend_log_mut(&mut self) -> Option<&mut TrendLog> {
        match &mut self.data {
            ObjectData::TrendLog(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_notification_class(&self) -> Option<&NotificationClass> {
        match &self.data {
            ObjectData::NotificationClass(n) => Some(n),
            _ => None,
        }
    }

    pub fn as_notification_class_mut(&mut self) -> Option<&mut NotificationClass> {
        match &mut self.data {
            ObjectData::NotificationClass(n) => Some(n),
            _ => None,
        }
    }

    /// Event_State: the reporting state for points that report, otherwise
    /// fault while unreliable and normal the rest of the time.
    pub fn event_state(&self) -> EventState {
        match &self.reporting {
            Some(r) => r.state,
            None if self.fault() => EventState::Fault,
            None => EventState::Normal,
        }
    }

    /// An event transition a BAS hasn't acknowledged yet.
    pub fn unacknowledged(&self) -> bool {
        self.reporting.as_ref().is_some_and(|r| r.unacknowledged())
    }

    /// Re-evaluates a schedule's present value after its configuration
    /// changes, unless it's out of service.
    pub fn refresh_schedule(&mut self, now: NaiveDateTime) {
        if let ObjectData::Schedule(s) = &self.data
            && !self.out_of_service
        {
            self.present_value = PointValue::Binary(s.value_at(now));
        }
    }

    /// The value the priority array resolves to, or the present value for
    /// objects without one.
    pub fn effective(&self) -> PointValue {
        if self.commandable {
            self.priority_array
                .iter()
                .flatten()
                .next()
                .copied()
                .unwrap_or(self.relinquish_default)
        } else {
            self.present_value
        }
    }

    /// The highest (lowest-numbered, 1-based) priority holding a value.
    pub fn active_priority(&self) -> Option<u8> {
        if !self.commandable {
            return None;
        }
        self.priority_array
            .iter()
            .position(Option::is_some)
            .map(|i| i as u8 + 1)
    }

    /// Commanded by something other than the simulation's own logic.
    pub fn is_overridden(&self) -> bool {
        self.active_priority().is_some()
    }

    pub fn fault(&self) -> bool {
        self.reliability != Reliability::NoFaultDetected
    }

    /// status-flags: in-alarm, fault, overridden, out-of-service.
    pub fn status_flags(&self) -> [bool; 4] {
        [self.in_alarm, self.fault(), false, self.out_of_service]
    }

    pub fn number_of_states(&self) -> u32 {
        self.states.len() as u32
    }

    fn refresh(&mut self) {
        if self.commandable {
            self.present_value = self.effective();
        }
    }

    /// Checks a value's kind and range against this object, converting
    /// numeric kinds where BACnet clients commonly mix them.
    pub fn coerce(&self, value: PointValue) -> Result<PointValue, WriteError> {
        let t = self.object_type;
        let v = if t.is_analog() {
            let x = match value {
                PointValue::Real(x) => x,
                PointValue::Multi(m) => m as f32,
                PointValue::Binary(_) => return Err(WriteError::InvalidDataType),
            };
            if !x.is_finite()
                || self.min_value.is_some_and(|min| x < min)
                || self.max_value.is_some_and(|max| x > max)
            {
                return Err(WriteError::ValueOutOfRange);
            }
            PointValue::Real(x)
        } else if t.is_binary() || t == ObjectType::Schedule {
            match value {
                PointValue::Binary(b) => PointValue::Binary(b),
                PointValue::Multi(0) => PointValue::Binary(false),
                PointValue::Multi(1) => PointValue::Binary(true),
                PointValue::Multi(_) => return Err(WriteError::ValueOutOfRange),
                PointValue::Real(_) => return Err(WriteError::InvalidDataType),
            }
        } else {
            match value {
                PointValue::Multi(m) if m >= 1 && m <= self.number_of_states() => {
                    PointValue::Multi(m)
                }
                PointValue::Multi(_) => return Err(WriteError::ValueOutOfRange),
                _ => return Err(WriteError::InvalidDataType),
            }
        };
        Ok(v)
    }

    /// WriteProperty on present-value. `value` None is a BACnet NULL
    /// (relinquish); `priority` defaults to 16.
    pub fn write_present_value(
        &mut self,
        value: Option<PointValue>,
        priority: Option<u8>,
    ) -> Result<(), WriteError> {
        if self.commandable {
            let p = priority.unwrap_or(16);
            if !(1..=16).contains(&p) || p == 6 {
                // priority 6 is reserved for minimum on/off
                return Err(WriteError::ValueOutOfRange);
            }
            let v = value.map(|v| self.coerce(v)).transpose()?;
            self.priority_array[p as usize - 1] = v;
            self.refresh();
            return Ok(());
        }
        let Some(value) = value else {
            return Err(WriteError::InvalidDataType);
        };
        if self.object_type.is_input() && !self.out_of_service {
            return Err(WriteError::WriteAccessDenied);
        }
        if !self.object_type.is_input() && !self.writable && !self.out_of_service {
            return Err(WriteError::WriteAccessDenied);
        }
        self.present_value = self.coerce(value)?;
        Ok(())
    }

    /// WriteProperty on relinquish-default (commandable objects only).
    pub fn write_relinquish_default(&mut self, value: PointValue) -> Result<(), WriteError> {
        if !self.commandable {
            return Err(WriteError::WriteAccessDenied);
        }
        self.relinquish_default = self.coerce(value)?;
        self.refresh();
        Ok(())
    }

    /// Display text for the present value, with units or state text.
    pub fn display_value(&self) -> String {
        format_value(self, self.present_value)
    }
}

/// Text for a value of `obj`'s kind: `72.4 °F`, `On`, `Occupied`.
pub fn format_value(obj: &BacObject, value: PointValue) -> String {
    match value {
        PointValue::Real(v) => {
            let d = obj.units.decimals();
            let sym = obj.units.symbol();
            if sym.is_empty() {
                format!("{v:.d$}")
            } else {
                format!("{v:.d$} {sym}")
            }
        }
        PointValue::Binary(b) => obj.states.get(b as usize).cloned().unwrap_or_else(|| {
            if b {
                "Active".into()
            } else {
                "Inactive".into()
            }
        }),
        PointValue::Multi(m) => obj
            .states
            .get((m as usize).wrapping_sub(1))
            .cloned()
            .unwrap_or_else(|| format!("State {m}")),
    }
}

#[derive(Clone, Debug)]
pub struct BacDevice {
    pub instance: u32,
    pub name: String,
    pub description: String,
    pub location: String,
    pub model_name: String,
    /// MAC on the simulator's virtual BACnet network, or None for the site
    /// controller that sits directly on BACnet/IP and routes to the rest.
    pub mac: Option<u8>,
    /// False while a scenario has it off the network: it answers nothing.
    pub online: bool,
    pub objects: Vec<BacObject>,
    pub database_revision: u32,
}

impl BacDevice {
    pub fn find(&self, object_type: ObjectType, instance: u32) -> Option<usize> {
        self.objects
            .iter()
            .position(|o| o.object_type == object_type && o.instance == instance)
    }

    pub fn find_by_name(&self, name: &str) -> Option<usize> {
        self.objects.iter().position(|o| o.name == name)
    }

    /// The notification class object numbered `number`.
    pub fn notification_class(&self, number: u32) -> Option<&NotificationClass> {
        self.objects
            .iter()
            .find(|o| o.object_type == ObjectType::NotificationClass && o.instance == number)
            .and_then(BacObject::as_notification_class)
    }
}

/// A point's place in the database, handed out when the engine builds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PointRef {
    pub device: u16,
    pub object: u16,
}

#[derive(Clone, Debug)]
pub struct PointDb {
    pub vendor_name: String,
    pub vendor_id: u16,
    pub firmware_revision: String,
    pub application_version: String,
    /// The virtual network number the routed devices sit on.
    pub network: u16,
    pub devices: Vec<BacDevice>,
    /// The simulated building's local time, set by [`PointDb::tick`]. None
    /// until the first tick: devices report the host's clock.
    pub now: Option<NaiveDateTime>,
    /// Event notifications waiting for the BACnet server.
    notices: VecDeque<EventNotice>,
}

/// A gap between ticks longer than this is a jump of the clock, not a step.
const LEAP: TimeDelta = TimeDelta::seconds(60);
/// Notifications kept for a server that isn't draining them.
const MAX_NOTICES: usize = 256;

impl PointDb {
    pub fn new(network: u16) -> Self {
        PointDb {
            vendor_name: "BASidekick".into(),
            vendor_id: 999,
            firmware_revision: "1.0".into(),
            application_version: env!("CARGO_PKG_VERSION").into(),
            network,
            devices: Vec::new(),
            now: None,
            notices: VecDeque::new(),
        }
    }

    /// The time to stamp things with: the sim's, or the host's before the
    /// first tick.
    pub fn time(&self) -> NaiveDateTime {
        self.now
            .unwrap_or_else(|| chrono::Local::now().naive_local())
    }

    /// One simulation step at the building's local time `now`: schedules
    /// update their present values, trend logs take the samples that are
    /// due, and intrinsic reporting runs. Cheap enough to call every step.
    pub fn tick(&mut self, now: NaiveDateTime) {
        let prev = self.now.replace(now);
        let leap = prev.is_some_and(|p| now < p || now - p > LEAP);
        let mut notices = Vec::new();
        for dev in &mut self.devices {
            let device = dev.instance;
            for i in 0..dev.objects.len() {
                let o = &mut dev.objects[i];
                match &mut o.data {
                    ObjectData::Point | ObjectData::NotificationClass(_) => {}
                    ObjectData::Schedule(s) => {
                        if !o.out_of_service {
                            o.present_value = PointValue::Binary(s.value_at(now));
                        }
                    }
                    ObjectData::TrendLog(t) => {
                        if let Some(at) = t.poll(now, leap) {
                            let (ty, instance) = t.target;
                            let sample = dev.find(ty, instance).map(|j| {
                                let x = &dev.objects[j];
                                (LogDatum::of(x.effective()), x.status_flags())
                            });
                            if let (Some((datum, flags)), Some(t)) =
                                (sample, dev.objects[i].as_trend_log_mut())
                            {
                                t.push(at, datum, Some(flags));
                            }
                        }
                    }
                }

                let o = &mut dev.objects[i];
                let (value, fault) = (o.effective(), o.fault());
                let Some(r) = o.reporting.as_deref_mut() else {
                    continue;
                };
                let transition = r.evaluate(value, fault, now);
                o.in_alarm = r.state != EventState::Normal;
                let Some((from, to)) = transition else {
                    continue;
                };
                let (class, t) = (r.notification_class, Transition::to(to).index());
                let enabled = r.event_enable[t];
                let nc = dev
                    .notification_class(class)
                    .map(|nc| (nc.priority[t], nc.ack_required[t]));
                let o = &mut dev.objects[i];
                if let Some(r) = o.reporting.as_deref_mut() {
                    r.acked[t] = !nc.is_some_and(|(_, ack)| ack);
                }
                // Without its notification class there's no one to tell.
                if let Some(nc) = nc.filter(|_| enabled) {
                    notices.extend(event::notice(device, o, (from, to), now, nc));
                }
            }
        }
        for n in notices {
            self.queue(n);
        }
    }

    fn queue(&mut self, n: EventNotice) {
        if self.notices.len() >= MAX_NOTICES {
            self.notices.pop_front();
        }
        self.notices.push_back(n);
    }

    /// Notifications waiting to be sent, oldest first, leaving none.
    pub fn take_notices(&mut self) -> Vec<EventNotice> {
        self.notices.drain(..).collect()
    }

    /// Acknowledges the transition into `state` of a point that reports
    /// events (a BAS's AcknowledgeAlarm, or the app's own button), and queues
    /// the acknowledgment notification. False if there was nothing to
    /// acknowledge.
    pub fn acknowledge(&mut self, r: PointRef, state: EventState) -> bool {
        let t = Transition::to(state);
        let dev = &self.devices[r.device as usize];
        let o = &dev.objects[r.object as usize];
        let Some(rep) = o.reporting.as_deref() else {
            return false;
        };
        let (Some(time), false) = (rep.time_stamps[t.index()], rep.acked[t.index()]) else {
            return false;
        };
        let priority = dev
            .notification_class(rep.notification_class)
            .map(|nc| nc.priority[t.index()]);
        let notice = EventNotice {
            device: dev.instance,
            object_type: o.object_type,
            instance: o.instance,
            time,
            notification_class: rep.notification_class,
            priority: priority.unwrap_or(255),
            event_type: rep.event_type(t),
            notify_type: NotifyType::AckNotification,
            ack_required: false,
            from: state,
            to: state,
            values: None,
            text: format!("{} {} acknowledged", o.name, state.label()),
        };
        let send = priority.is_some() && rep.event_enable[t.index()];
        if let Some(rep) = self.obj_mut(r).reporting.as_deref_mut() {
            rep.acked[t.index()] = true;
        }
        if send {
            self.queue(notice);
        }
        true
    }

    /// Points that report events and are out of normal or waiting for an
    /// acknowledgment, for the app's alarm list.
    pub fn alarms(&self) -> impl Iterator<Item = PointRef> + '_ {
        self.devices.iter().enumerate().flat_map(|(d, dev)| {
            dev.objects
                .iter()
                .enumerate()
                .filter(|(_, o)| {
                    o.reporting.is_some()
                        && (o.event_state() != EventState::Normal || o.unacknowledged())
                })
                .map(move |(o, _)| PointRef {
                    device: d as u16,
                    object: o as u16,
                })
        })
    }

    pub fn obj(&self, r: PointRef) -> &BacObject {
        &self.devices[r.device as usize].objects[r.object as usize]
    }

    pub fn obj_mut(&mut self, r: PointRef) -> &mut BacObject {
        &mut self.devices[r.device as usize].objects[r.object as usize]
    }

    /// The value control logic acts on: the resolved priority array for
    /// commandable objects, the (possibly client-written) present value
    /// otherwise.
    pub fn read(&self, r: PointRef) -> PointValue {
        self.obj(r).effective()
    }

    pub fn read_f64(&self, r: PointRef) -> f64 {
        self.read(r).as_f64()
    }

    pub fn read_bool(&self, r: PointRef) -> bool {
        self.read(r).as_bool()
    }

    pub fn read_u32(&self, r: PointRef) -> u32 {
        self.read(r).as_u32()
    }

    /// A measurement or status from the simulated physics. Ignored while the
    /// object is out of service, so a client's written value sticks.
    pub fn set_input(&mut self, r: PointRef, value: PointValue) {
        let o = self.obj_mut(r);
        if !o.out_of_service {
            o.present_value = value;
        }
    }

    pub fn set_real(&mut self, r: PointRef, value: f64) {
        self.set_input(r, PointValue::Real(value as f32));
    }

    pub fn set_bool(&mut self, r: PointRef, value: bool) {
        self.set_input(r, PointValue::Binary(value));
    }

    /// What the simulated controller's own program commands. Commandable
    /// objects take it as their relinquish default, so any client priority
    /// still wins.
    pub fn set_program(&mut self, r: PointRef, value: PointValue) {
        let o = self.obj_mut(r);
        if o.commandable {
            o.relinquish_default = value;
            o.refresh();
        } else if !o.out_of_service {
            o.present_value = value;
        }
    }

    pub fn set_reliability(&mut self, r: PointRef, reliability: Reliability) {
        self.obj_mut(r).reliability = reliability;
    }

    /// The engine's own alarm flag. Points with intrinsic reporting ignore
    /// it: theirs follows the event state.
    pub fn set_alarm(&mut self, r: PointRef, in_alarm: bool) {
        let o = self.obj_mut(r);
        if o.reporting.is_none() {
            o.in_alarm = in_alarm;
        }
    }

    pub fn device_index(&self, instance: u32) -> Option<usize> {
        self.devices.iter().position(|d| d.instance == instance)
    }

    /// The routed device at `mac` on the virtual network.
    pub fn device_at_mac(&self, mac: u8) -> Option<usize> {
        self.devices.iter().position(|d| d.mac == Some(mac))
    }

    /// The device on BACnet/IP itself (the site controller / router).
    pub fn local_device(&self) -> Option<usize> {
        self.devices.iter().position(|d| d.mac.is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_array_overrides_program_value() {
        let mut o = BacObject::new(
            ObjectType::AnalogOutput,
            1,
            "CLG-VLV",
            PointValue::Real(0.0),
        );
        o.write_relinquish_default(PointValue::Real(40.0)).unwrap();
        assert_eq!(o.effective(), PointValue::Real(40.0));
        o.write_present_value(Some(PointValue::Real(100.0)), Some(8))
            .unwrap();
        assert_eq!(o.present_value, PointValue::Real(100.0));
        assert_eq!(o.active_priority(), Some(8));
        o.write_present_value(None, Some(8)).unwrap();
        assert_eq!(o.present_value, PointValue::Real(40.0));
        assert!(
            o.write_present_value(Some(PointValue::Real(1.0)), Some(6))
                .is_err()
        );
    }

    #[test]
    fn inputs_need_out_of_service() {
        let mut o = BacObject::new(ObjectType::AnalogInput, 1, "ZN-T", PointValue::Real(72.0));
        assert_eq!(
            o.write_present_value(Some(PointValue::Real(60.0)), None),
            Err(WriteError::WriteAccessDenied)
        );
        o.out_of_service = true;
        o.write_present_value(Some(PointValue::Real(60.0)), None)
            .unwrap();
        assert_eq!(o.effective(), PointValue::Real(60.0));
    }

    #[test]
    fn multistate_range_checked() {
        let mut o = BacObject::new(
            ObjectType::MultiStateValue,
            1,
            "OCC-MODE",
            PointValue::Multi(1),
        );
        o.commandable = true;
        o.states = vec!["Auto".into(), "Occupied".into(), "Unoccupied".into()];
        assert_eq!(
            o.write_present_value(Some(PointValue::Multi(4)), Some(8)),
            Err(WriteError::ValueOutOfRange)
        );
        o.write_present_value(Some(PointValue::Multi(2)), Some(8))
            .unwrap();
        assert_eq!(o.effective(), PointValue::Multi(2));
    }

    fn at(h: u32, m: u32, s: u32) -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 11, 2) // a Monday
            .unwrap()
            .and_hms_opt(h, m, s)
            .unwrap()
    }

    /// One device: a zone temp with high/low alarms, a fan alarm, their
    /// notification class, a schedule and a trend of the zone temp.
    fn building() -> PointDb {
        let hm = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
        let day = vec![(hm(7, 0), true), (hm(17, 0), false)];
        let mut zn_t = BacObject::new(ObjectType::AnalogInput, 1, "ZN-T", PointValue::Real(72.0));
        zn_t.units = Units::DegreesFahrenheit;
        zn_t.enable_out_of_range(60.0, 80.0, 1.0, 5, 120);
        let mut alarm = BacObject::new(
            ObjectType::BinaryInput,
            1,
            "SF-FAIL",
            PointValue::Binary(false),
        );
        alarm.enable_change_of_state(true, 5, 0);
        let mut db = PointDb::new(2001);
        db.devices.push(BacDevice {
            instance: 1001,
            name: "RTU-1".into(),
            description: String::new(),
            location: String::new(),
            model_name: String::new(),
            mac: Some(1),
            online: true,
            objects: vec![
                zn_t,
                alarm,
                BacObject::notification_class(5, "ALARMS"),
                BacObject::schedule(
                    1,
                    "OCC-SCHED",
                    [
                        day.clone(),
                        day.clone(),
                        day.clone(),
                        day.clone(),
                        day,
                        vec![],
                        vec![],
                    ],
                ),
                BacObject::trend_log(1, "ZN-T-TL", (ObjectType::AnalogInput, 1), 300, 100),
            ],
            database_revision: 1,
        });
        db
    }

    const ZN_T: PointRef = PointRef {
        device: 0,
        object: 0,
    };
    const FAIL: PointRef = PointRef {
        device: 0,
        object: 1,
    };
    const SCHED: PointRef = PointRef {
        device: 0,
        object: 3,
    };
    const TREND: PointRef = PointRef {
        device: 0,
        object: 4,
    };

    #[test]
    fn tick_runs_schedules_and_trends_on_sim_time() {
        let mut db = building();
        assert!(db.now.is_none());
        db.tick(at(6, 59, 0));
        assert_eq!(db.read(SCHED), PointValue::Binary(false));
        db.tick(at(7, 0, 0));
        assert_eq!(db.read(SCHED), PointValue::Binary(true));
        assert_eq!(db.now, Some(at(7, 0, 0)));
        // Out of service holds a written value.
        db.obj_mut(SCHED).out_of_service = true;
        db.obj_mut(SCHED)
            .write_present_value(Some(PointValue::Multi(0)), None)
            .unwrap();
        db.tick(at(7, 0, 1));
        assert_eq!(db.read(SCHED), PointValue::Binary(false));
        let s = db.obj(SCHED).as_schedule().unwrap();
        assert_eq!(
            s.next_change(at(7, 0, 1), chrono::Duration::hours(12)),
            Some((at(17, 0, 0), false))
        );

        // The 07:00 tick took the first sample; one-second steps for twelve
        // minutes add 07:05 and 07:10.
        let mut t = at(7, 0, 1);
        while t < at(7, 12, 0) {
            db.set_real(ZN_T, 70.0 + (t - at(7, 0, 0)).num_seconds() as f64 / 60.0);
            db.tick(t);
            t += chrono::Duration::seconds(1);
        }
        let log = db.obj(TREND).as_trend_log().unwrap();
        assert_eq!(
            log.records().collect::<Vec<_>>(),
            [
                (at(7, 0, 0), 72.0),
                (at(7, 5, 0), 75.0),
                (at(7, 10, 0), 80.0)
            ]
        );
        assert_eq!(log.buffer()[0].flags, Some([false; 4]));
        // Jumping ahead a day leaves one status record, not 288 samples.
        let mut t = at(7, 12, 0) + chrono::Duration::days(1);
        while t <= at(7, 15, 0) + chrono::Duration::days(1) {
            db.tick(t);
            t += chrono::Duration::seconds(5);
        }
        let log = db.obj(TREND).as_trend_log().unwrap();
        assert_eq!(log.record_count(), 5);
        assert!(matches!(log.buffer()[3].datum, LogDatum::Status(_)));
        assert_eq!(
            log.buffer()[4].time,
            at(7, 15, 0) + chrono::Duration::days(1)
        );
    }

    #[test]
    fn intrinsic_reporting_queues_notices_and_takes_acks() {
        let mut db = building();
        db.tick(at(8, 0, 0));
        assert!(db.take_notices().is_empty());

        // High for two minutes of sim time before it alarms.
        db.set_real(ZN_T, 82.0);
        db.tick(at(8, 0, 1));
        db.tick(at(8, 2, 0));
        assert_eq!(db.obj(ZN_T).event_state(), EventState::Normal);
        db.tick(at(8, 2, 1));
        assert_eq!(db.obj(ZN_T).event_state(), EventState::HighLimit);
        assert!(db.obj(ZN_T).in_alarm && db.obj(ZN_T).unacknowledged());
        // The engine's own alarm flag doesn't fight the event state.
        db.set_alarm(ZN_T, false);
        assert!(db.obj(ZN_T).in_alarm);
        let n = db.take_notices();
        assert_eq!(n.len(), 1);
        assert_eq!(
            (n[0].from, n[0].to, n[0].event_type, n[0].priority),
            (
                EventState::Normal,
                EventState::HighLimit,
                EventType::OutOfRange,
                100
            )
        );
        assert!(n[0].ack_required);
        assert_eq!(n[0].time, at(8, 2, 1));
        assert_eq!(n[0].text, "ZN-T high limit: 82.0 °F");
        assert_eq!(
            n[0].values,
            Some(EventValues::OutOfRange {
                value: 82.0,
                flags: [true, false, false, false],
                deadband: 1.0,
                limit: 80.0
            })
        );
        assert_eq!(db.alarms().collect::<Vec<_>>(), [ZN_T]);

        // A fault comes through at once, as a change of reliability.
        db.set_reliability(FAIL, Reliability::OpenLoop);
        db.tick(at(8, 3, 0));
        assert_eq!(db.obj(FAIL).event_state(), EventState::Fault);
        let n = db.take_notices();
        assert_eq!(n[0].event_type, EventType::ChangeOfReliability);
        assert_eq!(n[0].text, "SF-FAIL fault: open loop");

        // Acknowledging queues an ack notification; a second ack is a no-op.
        assert!(db.acknowledge(ZN_T, EventState::HighLimit));
        assert!(!db.acknowledge(ZN_T, EventState::HighLimit));
        assert!(!db.obj(ZN_T).unacknowledged());
        let n = db.take_notices();
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].notify_type, NotifyType::AckNotification);
        assert_eq!(n[0].time, at(8, 2, 1));
        assert!(n[0].values.is_none());

        // Back to normal (no ack needed by default), and still listed while
        // the fault's unacknowledged.
        db.set_real(ZN_T, 72.0);
        db.tick(at(8, 4, 0));
        db.tick(at(8, 6, 0));
        assert_eq!(db.obj(ZN_T).event_state(), EventState::Normal);
        assert!(!db.obj(ZN_T).in_alarm && !db.obj(ZN_T).unacknowledged());
        assert_eq!(db.take_notices()[0].to, EventState::Normal);
        assert_eq!(db.alarms().collect::<Vec<_>>(), [FAIL]);

        // Points without reporting: normal, or fault while unreliable.
        let plain = PointRef {
            device: 0,
            object: 3,
        };
        assert_eq!(db.obj(plain).event_state(), EventState::Normal);
        db.set_reliability(plain, Reliability::UnreliableOther);
        assert_eq!(db.obj(plain).event_state(), EventState::Fault);
    }
}
