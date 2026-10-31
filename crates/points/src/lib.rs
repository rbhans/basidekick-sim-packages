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

use parking_lot::Mutex;
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
}

impl ObjectType {
    pub const ALL: [ObjectType; 9] = [
        ObjectType::AnalogInput,
        ObjectType::AnalogOutput,
        ObjectType::AnalogValue,
        ObjectType::BinaryInput,
        ObjectType::BinaryOutput,
        ObjectType::BinaryValue,
        ObjectType::MultiStateInput,
        ObjectType::MultiStateOutput,
        ObjectType::MultiStateValue,
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
        } else if t.is_binary() {
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
}

impl PointDb {
    pub fn new(network: u16) -> Self {
        PointDb {
            vendor_name: "BASidekick".into(),
            vendor_id: 999,
            firmware_revision: "1.0".into(),
            application_version: env!("CARGO_PKG_VERSION").into(),
            network,
            devices: Vec::new(),
        }
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

    pub fn set_alarm(&mut self, r: PointRef, in_alarm: bool) {
        self.obj_mut(r).in_alarm = in_alarm;
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
}
