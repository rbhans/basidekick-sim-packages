//! Property values for the device and objects a simulated controller serves.
//!
//! Which properties an object has depends on its type and on the object
//! itself: min/max-pres-value only when limits are set, the priority array
//! only when commandable, the alarm properties only with intrinsic reporting.
//! That one list drives everything else: reads of ALL/REQUIRED/OPTIONAL,
//! property-list, and unknown-property errors. Schedule, trend log and
//! alarm properties are encoded in their own modules.

use crate::apdu::{BacError, MAX_APDU};
use crate::encoding::*;
use crate::{alarm, clock, schedule, trend};
use bask_points::{BacDevice, BacObject, EventAlgorithm, ObjectType, PointDb, PointValue};

/// BACnetPropertyIdentifier values used here.
pub mod prop {
    pub const ACKED_TRANSITIONS: u32 = 0;
    pub const ACK_REQUIRED: u32 = 1;
    pub const ACTIVE_TEXT: u32 = 4;
    pub const ALARM_VALUE: u32 = 6;
    pub const ALL: u32 = 8;
    pub const APDU_TIMEOUT: u32 = 11;
    pub const APPLICATION_SOFTWARE_VERSION: u32 = 12;
    pub const NOTIFICATION_CLASS: u32 = 17;
    pub const COV_INCREMENT: u32 = 22;
    pub const DAYLIGHT_SAVINGS_STATUS: u32 = 24;
    pub const DEADBAND: u32 = 25;
    pub const DESCRIPTION: u32 = 28;
    pub const DEVICE_ADDRESS_BINDING: u32 = 30;
    pub const EFFECTIVE_PERIOD: u32 = 32;
    pub const EVENT_ENABLE: u32 = 35;
    pub const EVENT_STATE: u32 = 36;
    pub const EXCEPTION_SCHEDULE: u32 = 38;
    pub const FIRMWARE_REVISION: u32 = 44;
    pub const HIGH_LIMIT: u32 = 45;
    pub const INACTIVE_TEXT: u32 = 46;
    pub const LIMIT_ENABLE: u32 = 52;
    pub const LIST_OF_OBJECT_PROPERTY_REFERENCES: u32 = 54;
    pub const LOCAL_DATE: u32 = 56;
    pub const LOCAL_TIME: u32 = 57;
    pub const LOCATION: u32 = 58;
    pub const LOW_LIMIT: u32 = 59;
    pub const MAX_APDU_LENGTH_ACCEPTED: u32 = 62;
    pub const MAX_PRES_VALUE: u32 = 65;
    pub const MIN_PRES_VALUE: u32 = 69;
    pub const MODEL_NAME: u32 = 70;
    pub const NOTIFY_TYPE: u32 = 72;
    pub const NUMBER_OF_APDU_RETRIES: u32 = 73;
    pub const NUMBER_OF_STATES: u32 = 74;
    pub const OBJECT_IDENTIFIER: u32 = 75;
    pub const OBJECT_LIST: u32 = 76;
    pub const OBJECT_NAME: u32 = 77;
    pub const OBJECT_TYPE: u32 = 79;
    pub const OPTIONAL: u32 = 80;
    pub const OUT_OF_SERVICE: u32 = 81;
    pub const POLARITY: u32 = 84;
    pub const PRESENT_VALUE: u32 = 85;
    pub const PRIORITY: u32 = 86;
    pub const PRIORITY_ARRAY: u32 = 87;
    pub const PRIORITY_FOR_WRITING: u32 = 88;
    pub const PROTOCOL_OBJECT_TYPES_SUPPORTED: u32 = 96;
    pub const PROTOCOL_SERVICES_SUPPORTED: u32 = 97;
    pub const PROTOCOL_VERSION: u32 = 98;
    pub const RECIPIENT_LIST: u32 = 102;
    pub const RELIABILITY: u32 = 103;
    pub const RELINQUISH_DEFAULT: u32 = 104;
    pub const REQUIRED: u32 = 105;
    pub const SEGMENTATION_SUPPORTED: u32 = 107;
    pub const STATE_TEXT: u32 = 110;
    pub const STATUS_FLAGS: u32 = 111;
    pub const SYSTEM_STATUS: u32 = 112;
    pub const TIME_DELAY: u32 = 113;
    pub const UNITS: u32 = 117;
    pub const UTC_OFFSET: u32 = 119;
    pub const VENDOR_IDENTIFIER: u32 = 120;
    pub const VENDOR_NAME: u32 = 121;
    pub const WEEKLY_SCHEDULE: u32 = 123;
    pub const BUFFER_SIZE: u32 = 126;
    pub const EVENT_TIME_STAMPS: u32 = 130;
    pub const LOG_BUFFER: u32 = 131;
    pub const LOG_DEVICE_OBJECT_PROPERTY: u32 = 132;
    pub const ENABLE: u32 = 133;
    pub const LOG_INTERVAL: u32 = 134;
    pub const PROTOCOL_REVISION: u32 = 139;
    pub const RECORD_COUNT: u32 = 141;
    pub const STOP_WHEN_FULL: u32 = 144;
    pub const TOTAL_RECORD_COUNT: u32 = 145;
    pub const DATABASE_REVISION: u32 = 155;
    pub const SCHEDULE_DEFAULT: u32 = 174;
    pub const LOGGING_TYPE: u32 = 197;
    pub const EVENT_DETECTION_ENABLE: u32 = 353;
    pub const PROPERTY_LIST: u32 = 371;
    pub const CURRENT_COMMAND_PRIORITY: u32 = 431;
}

use prop::*;

/// segmentation-supported: no-segmentation.
pub const SEGMENTATION_NONE: u32 = 3;

/// BACnetServicesSupported bits for what the devices execute: the confirmed
/// services they answer, the unconfirmed ones they act on, and the
/// notifications they send.
const SERVICES_SUPPORTED: [usize; 18] = [
    0,  // acknowledgeAlarm
    1,  // confirmedCOVNotification
    2,  // confirmedEventNotification
    5,  // subscribeCOV
    12, // readProperty
    14, // readPropertyMultiple
    15, // writeProperty
    16, // writePropertyMultiple
    17, // deviceCommunicationControl
    20, // reinitializeDevice
    26, // i-Am
    27, // i-Have
    28, // unconfirmedCOVNotification
    29, // unconfirmedEventNotification
    33, // who-Has
    34, // who-Is
    35, // readRange
    39, // getEventInformation
];
/// Bit count for Protocol_Revision 14 (through getEventInformation, writeGroup).
const SERVICES_BITS: usize = 41;
/// Bit count for Protocol_Revision 14 object types (through lighting-output).
const OBJECT_TYPES_BITS: usize = 55;

/// What an object identifier in a request names on the addressed device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Device,
    Object(usize),
}

pub fn device_id(d: &BacDevice) -> ObjectId {
    ObjectId::new(ObjectId::DEVICE, d.instance)
}

pub fn object_id(o: &BacObject) -> ObjectId {
    ObjectId::new(o.object_type.bacnet_id(), o.instance)
}

/// The addressed device's own instance in place of the wildcard.
pub fn unwildcard(d: &BacDevice, id: ObjectId) -> ObjectId {
    if id.object_type == ObjectId::DEVICE && id.instance == ObjectId::WILDCARD {
        device_id(d)
    } else {
        id
    }
}

pub fn resolve(d: &BacDevice, id: ObjectId) -> Result<Target, BacError> {
    let id = unwildcard(d, id);
    if id == device_id(d) {
        return Ok(Target::Device);
    }
    ObjectType::from_bacnet_id(id.object_type)
        .and_then(|t| d.find(t, id.instance))
        .map(Target::Object)
        .ok_or(BacError::UNKNOWN_OBJECT)
}

/// The properties `target` has: (required, optional).
pub fn properties(d: &BacDevice, target: Target) -> (Vec<u32>, Vec<u32>) {
    let o = match target {
        Target::Device => {
            return (
                vec![
                    OBJECT_IDENTIFIER,
                    OBJECT_NAME,
                    OBJECT_TYPE,
                    SYSTEM_STATUS,
                    VENDOR_NAME,
                    VENDOR_IDENTIFIER,
                    MODEL_NAME,
                    FIRMWARE_REVISION,
                    APPLICATION_SOFTWARE_VERSION,
                    PROTOCOL_VERSION,
                    PROTOCOL_REVISION,
                    PROTOCOL_SERVICES_SUPPORTED,
                    PROTOCOL_OBJECT_TYPES_SUPPORTED,
                    OBJECT_LIST,
                    MAX_APDU_LENGTH_ACCEPTED,
                    SEGMENTATION_SUPPORTED,
                    APDU_TIMEOUT,
                    NUMBER_OF_APDU_RETRIES,
                    DEVICE_ADDRESS_BINDING,
                    DATABASE_REVISION,
                    PROPERTY_LIST,
                ],
                vec![
                    LOCATION,
                    DESCRIPTION,
                    LOCAL_DATE,
                    LOCAL_TIME,
                    UTC_OFFSET,
                    DAYLIGHT_SAVINGS_STATUS,
                ],
            );
        }
        Target::Object(i) => &d.objects[i],
    };
    let t = o.object_type;
    let mut required = vec![OBJECT_IDENTIFIER, OBJECT_NAME, OBJECT_TYPE];
    let mut optional = vec![DESCRIPTION];
    match t {
        ObjectType::Schedule => {
            required.extend([
                PRESENT_VALUE,
                EFFECTIVE_PERIOD,
                SCHEDULE_DEFAULT,
                LIST_OF_OBJECT_PROPERTY_REFERENCES,
                PRIORITY_FOR_WRITING,
                STATUS_FLAGS,
                RELIABILITY,
                OUT_OF_SERVICE,
            ]);
            optional.extend([WEEKLY_SCHEDULE, EXCEPTION_SCHEDULE, EVENT_STATE]);
        }
        ObjectType::TrendLog => {
            required.extend([
                ENABLE,
                STOP_WHEN_FULL,
                BUFFER_SIZE,
                LOG_BUFFER,
                RECORD_COUNT,
                TOTAL_RECORD_COUNT,
                LOGGING_TYPE,
                STATUS_FLAGS,
                EVENT_STATE,
            ]);
            optional.extend([LOG_DEVICE_OBJECT_PROPERTY, LOG_INTERVAL]);
        }
        ObjectType::NotificationClass => {
            required.extend([NOTIFICATION_CLASS, PRIORITY, ACK_REQUIRED, RECIPIENT_LIST]);
        }
        _ => {
            required.extend([PRESENT_VALUE, STATUS_FLAGS, EVENT_STATE, OUT_OF_SERVICE]);
            optional.push(RELIABILITY);
            point_properties(o, &mut required, &mut optional);
        }
    }
    required.push(PROPERTY_LIST);
    (required, optional)
}

/// The type-dependent properties of the value objects.
fn point_properties(o: &BacObject, required: &mut Vec<u32>, optional: &mut Vec<u32>) {
    let t = o.object_type;
    if t.is_analog() {
        required.push(UNITS);
        optional.push(COV_INCREMENT);
        if o.min_value.is_some() {
            optional.push(MIN_PRES_VALUE);
        }
        if o.max_value.is_some() {
            optional.push(MAX_PRES_VALUE);
        }
    }
    if matches!(t, ObjectType::BinaryInput | ObjectType::BinaryOutput) {
        required.push(POLARITY);
    }
    if t.is_binary() && o.states.len() >= 2 {
        optional.extend([INACTIVE_TEXT, ACTIVE_TEXT]);
    }
    if t.is_multistate() {
        required.push(NUMBER_OF_STATES);
        if !o.states.is_empty() {
            optional.push(STATE_TEXT);
        }
    }
    if o.commandable {
        required.extend([PRIORITY_ARRAY, RELINQUISH_DEFAULT, CURRENT_COMMAND_PRIORITY]);
    }
    if let Some(r) = &o.reporting {
        optional.extend([TIME_DELAY, NOTIFICATION_CLASS]);
        match r.algorithm {
            EventAlgorithm::OutOfRange { .. } => {
                optional.extend([HIGH_LIMIT, LOW_LIMIT, DEADBAND, LIMIT_ENABLE])
            }
            EventAlgorithm::ChangeOfState { .. } => optional.push(ALARM_VALUE),
        }
        optional.extend([
            EVENT_ENABLE,
            ACKED_TRANSITIONS,
            NOTIFY_TYPE,
            EVENT_TIME_STAMPS,
            EVENT_DETECTION_ENABLE,
        ]);
    }
}

pub fn has_property(d: &BacDevice, target: Target, property: u32) -> bool {
    let (required, optional) = properties(d, target);
    required.contains(&property) || optional.contains(&property)
}

/// Property-list leaves out the four properties every object has.
fn property_list(d: &BacDevice, target: Target) -> Vec<u32> {
    let (required, optional) = properties(d, target);
    required
        .into_iter()
        .chain(optional)
        .filter(|p| {
            !matches!(
                *p,
                OBJECT_IDENTIFIER | OBJECT_NAME | OBJECT_TYPE | PROPERTY_LIST
            )
        })
        .collect()
}

pub fn is_array(property: u32) -> bool {
    matches!(
        property,
        OBJECT_LIST
            | PRIORITY_ARRAY
            | STATE_TEXT
            | PROPERTY_LIST
            | WEEKLY_SCHEDULE
            | EXCEPTION_SCHEDULE
            | EVENT_TIME_STAMPS
            | PRIORITY
    )
}

/// Appends the application-tagged value of `property` (or one element of it)
/// to `buf`.
pub fn read(
    db: &PointDb,
    d: &BacDevice,
    target: Target,
    property: u32,
    index: Option<u64>,
    buf: &mut Vec<u8>,
) -> Result<(), BacError> {
    if !has_property(d, target, property) {
        return Err(BacError::UNKNOWN_PROPERTY);
    }
    // The log buffer is read with ReadRange only.
    if property == LOG_BUFFER {
        return Err(BacError::READ_ACCESS_DENIED);
    }
    if is_array(property) {
        let len = array_len(d, target, property);
        match index {
            None => (0..len).for_each(|i| element(d, target, property, i, buf)),
            Some(0) => app_unsigned(buf, len as u64),
            Some(n) if n <= len as u64 => element(d, target, property, n as usize - 1, buf),
            Some(_) => return Err(BacError::INVALID_ARRAY_INDEX),
        }
        return Ok(());
    }
    if index.is_some() {
        return Err(BacError::NOT_AN_ARRAY);
    }
    match target {
        Target::Device => read_device(db, d, property, buf),
        Target::Object(i) => read_object(&d.objects[i], property, buf),
    }
    Ok(())
}

pub fn array_len(d: &BacDevice, target: Target, property: u32) -> usize {
    match (target, property) {
        (_, PROPERTY_LIST) => property_list(d, target).len(),
        (Target::Device, _) => d.objects.len() + 1,
        (Target::Object(i), PRIORITY_ARRAY) => d.objects[i].priority_array.len(),
        (Target::Object(i), STATE_TEXT) => d.objects[i].states.len(),
        (Target::Object(i), _) => {
            let o = &d.objects[i];
            schedule::array_len(o, property)
                .or_else(|| alarm::array_len(o, property))
                .unwrap_or(0)
        }
    }
}

/// One element (0-based) of an array property.
fn element(d: &BacDevice, target: Target, property: u32, i: usize, buf: &mut Vec<u8>) {
    match (target, property) {
        (_, PROPERTY_LIST) => app_enumerated(buf, property_list(d, target)[i]),
        (Target::Device, _) if i == 0 => app_object_id(buf, device_id(d)),
        (Target::Device, _) => app_object_id(buf, object_id(&d.objects[i - 1])),
        (Target::Object(o), PRIORITY_ARRAY) => {
            let o = &d.objects[o];
            match o.priority_array[i] {
                Some(v) => encode_value(buf, o.object_type, v),
                None => app_null(buf),
            }
        }
        (Target::Object(o), STATE_TEXT) => app_char_string(buf, &d.objects[o].states[i]),
        (Target::Object(o), _) => {
            let o = &d.objects[o];
            if !schedule::element(o, property, i, buf) && !alarm::element(o, property, i, buf) {
                unreachable!("array {property} listed but not encoded");
            }
        }
    }
}

/// A present value in its object type's BACnet data type. Schedules hold
/// BACnetBinaryPV, like binary objects.
pub fn encode_value(buf: &mut Vec<u8>, t: ObjectType, v: PointValue) {
    if t.is_analog() {
        app_real(
            buf,
            if let PointValue::Real(x) = v {
                x
            } else {
                v.as_f64() as f32
            },
        );
    } else if t.is_multistate() {
        app_unsigned(buf, v.as_u32() as u64);
    } else {
        app_enumerated(buf, v.as_bool() as u32);
    }
}

fn read_object(o: &BacObject, property: u32, buf: &mut Vec<u8>) {
    match property {
        OBJECT_IDENTIFIER => app_object_id(buf, object_id(o)),
        OBJECT_NAME => app_char_string(buf, &o.name),
        OBJECT_TYPE => app_enumerated(buf, o.object_type.bacnet_id() as u32),
        PRESENT_VALUE => encode_value(buf, o.object_type, o.effective()),
        DESCRIPTION => app_char_string(buf, &o.description),
        STATUS_FLAGS => app_bit_string(buf, &o.status_flags()),
        EVENT_STATE => app_enumerated(buf, o.event_state().bacnet_id()),
        OUT_OF_SERVICE => app_bool(buf, o.out_of_service),
        RELIABILITY => app_enumerated(buf, o.reliability.bacnet_id()),
        UNITS => app_enumerated(buf, o.units.bacnet_id()),
        COV_INCREMENT => app_real(buf, o.cov_increment),
        MIN_PRES_VALUE => app_real(buf, o.min_value.unwrap_or_default()),
        MAX_PRES_VALUE => app_real(buf, o.max_value.unwrap_or_default()),
        POLARITY => app_enumerated(buf, 0), // normal
        INACTIVE_TEXT => app_char_string(buf, &o.states[0]),
        ACTIVE_TEXT => app_char_string(buf, &o.states[1]),
        NUMBER_OF_STATES => app_unsigned(buf, o.number_of_states() as u64),
        RELINQUISH_DEFAULT => encode_value(buf, o.object_type, o.relinquish_default),
        CURRENT_COMMAND_PRIORITY => match o.active_priority() {
            Some(p) => app_unsigned(buf, p as u64),
            None => app_null(buf),
        },
        _ => {
            if !schedule::read(o, property, buf)
                && !trend::read(o, property, buf)
                && !alarm::read(o, property, buf)
            {
                unreachable!("property {property} listed but not encoded")
            }
        }
    }
}

fn read_device(db: &PointDb, d: &BacDevice, property: u32, buf: &mut Vec<u8>) {
    match property {
        OBJECT_IDENTIFIER => app_object_id(buf, device_id(d)),
        OBJECT_NAME => app_char_string(buf, &d.name),
        OBJECT_TYPE => app_enumerated(buf, ObjectId::DEVICE as u32),
        SYSTEM_STATUS => app_enumerated(buf, 0), // operational
        VENDOR_NAME => app_char_string(buf, &db.vendor_name),
        VENDOR_IDENTIFIER => app_unsigned(buf, db.vendor_id as u64),
        MODEL_NAME => app_char_string(buf, &d.model_name),
        FIRMWARE_REVISION => app_char_string(buf, &db.firmware_revision),
        APPLICATION_SOFTWARE_VERSION => app_char_string(buf, &db.application_version),
        LOCATION => app_char_string(buf, &d.location),
        DESCRIPTION => app_char_string(buf, &d.description),
        PROTOCOL_VERSION => app_unsigned(buf, 1),
        PROTOCOL_REVISION => app_unsigned(buf, 14),
        PROTOCOL_SERVICES_SUPPORTED => {
            let mut bits = [false; SERVICES_BITS];
            SERVICES_SUPPORTED.iter().for_each(|&b| bits[b] = true);
            app_bit_string(buf, &bits);
        }
        PROTOCOL_OBJECT_TYPES_SUPPORTED => {
            let mut bits = [false; OBJECT_TYPES_BITS];
            ObjectType::ALL
                .iter()
                .for_each(|t| bits[t.bacnet_id() as usize] = true);
            bits[ObjectId::DEVICE as usize] = true;
            app_bit_string(buf, &bits);
        }
        MAX_APDU_LENGTH_ACCEPTED => app_unsigned(buf, MAX_APDU as u64),
        SEGMENTATION_SUPPORTED => app_enumerated(buf, SEGMENTATION_NONE),
        APDU_TIMEOUT => app_unsigned(buf, 3000),
        NUMBER_OF_APDU_RETRIES => app_unsigned(buf, 3),
        DEVICE_ADDRESS_BINDING => {} // an empty list
        DATABASE_REVISION => app_unsigned(buf, d.database_revision as u64),
        // The sim's clock once it runs, the host's before.
        LOCAL_DATE => app_date(buf, clock::device_time(db.now).date()),
        LOCAL_TIME => app_time(buf, clock::device_time(db.now).time()),
        UTC_OFFSET => app_signed(buf, clock::now().utc_offset as i64),
        DAYLIGHT_SAVINGS_STATUS => app_bool(buf, clock::now().dst),
        _ => unreachable!("property {property} listed but not encoded"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bask_points::Units;
    use chrono::{NaiveDate, NaiveTime};

    fn device() -> (PointDb, BacDevice) {
        let mut db = PointDb::new(2001);
        let mut av = bask_points::BacObject::new(
            ObjectType::AnalogValue,
            1,
            "ZN-SP",
            PointValue::Real(72.0),
        );
        av.commandable = true;
        av.units = Units::DegreesFahrenheit;
        av.min_value = Some(55.0);
        let mut msv = bask_points::BacObject::new(
            ObjectType::MultiStateValue,
            2,
            "OCC",
            PointValue::Multi(1),
        );
        msv.states = vec!["Occ".into(), "Unocc".into()];
        let mut bi = bask_points::BacObject::new(
            ObjectType::BinaryInput,
            3,
            "FAN-S",
            PointValue::Binary(true),
        );
        bi.enable_change_of_state(false, 1, 30);
        let mut ai =
            bask_points::BacObject::new(ObjectType::AnalogInput, 4, "ZN-T", PointValue::Real(71.0));
        ai.enable_out_of_range(60.0, 80.0, 1.0, 1, 30);
        let d = BacDevice {
            instance: 10,
            name: "VAV-1".into(),
            description: String::new(),
            location: String::new(),
            model_name: "Sim".into(),
            mac: Some(1),
            online: true,
            objects: vec![
                av,
                msv,
                bi,
                ai,
                BacObject::schedule(
                    1,
                    "SCHED",
                    std::array::from_fn(|_| {
                        vec![(NaiveTime::from_hms_opt(7, 0, 0).unwrap(), true)]
                    }),
                ),
                BacObject::trend_log(1, "TL", (ObjectType::AnalogInput, 4), 60, 10),
                BacObject::notification_class(1, "NC"),
            ],
            database_revision: 1,
        };
        db.devices.push(d.clone());
        (db, d)
    }

    fn value(
        db: &PointDb,
        d: &BacDevice,
        t: Target,
        p: u32,
        index: Option<u64>,
    ) -> Result<Vec<u8>, BacError> {
        let mut buf = Vec::new();
        read(db, d, t, p, index, &mut buf).map(|_| buf)
    }

    #[test]
    fn every_listed_property_reads() {
        let (db, d) = device();
        // Lists that start out empty.
        let empty = [
            DEVICE_ADDRESS_BINDING,
            LIST_OF_OBJECT_PROPERTY_REFERENCES,
            EXCEPTION_SCHEDULE,
        ];
        for t in std::iter::once(Target::Device).chain((0..d.objects.len()).map(Target::Object)) {
            let (req, opt) = properties(&d, t);
            for p in req.into_iter().chain(opt) {
                if p == LOG_BUFFER {
                    assert_eq!(
                        value(&db, &d, t, p, None),
                        Err(BacError::READ_ACCESS_DENIED)
                    );
                    continue;
                }
                let v = value(&db, &d, t, p, None).unwrap();
                assert!(!v.is_empty() || empty.contains(&p), "{t:?} {p}");
            }
        }
    }

    #[test]
    fn arrays_and_indexes() {
        let (db, d) = device();
        let av = Target::Object(0);
        assert_eq!(
            value(&db, &d, av, PRIORITY_ARRAY, Some(0)).unwrap(),
            [0x21, 16]
        );
        assert_eq!(
            value(&db, &d, av, PRIORITY_ARRAY, None).unwrap(),
            [0x00; 16]
        );
        assert_eq!(
            value(&db, &d, av, PRIORITY_ARRAY, Some(17)),
            Err(BacError::INVALID_ARRAY_INDEX)
        );
        assert_eq!(
            value(&db, &d, av, PRESENT_VALUE, Some(1)),
            Err(BacError::NOT_AN_ARRAY)
        );
        assert_eq!(
            value(&db, &d, av, CURRENT_COMMAND_PRIORITY, None).unwrap(),
            [0x00]
        );
        assert_eq!(
            value(&db, &d, Target::Object(1), STATE_TEXT, Some(2)).unwrap(),
            [0x75, 6, 0, b'U', b'n', b'o', b'c', b'c']
        );
        assert_eq!(
            value(&db, &d, Target::Object(2), PRIORITY_ARRAY, None),
            Err(BacError::UNKNOWN_PROPERTY)
        );
        assert_eq!(
            value(&db, &d, Target::Device, OBJECT_LIST, Some(0)).unwrap(),
            [0x21, 8]
        );
        assert_eq!(
            value(&db, &d, Target::Device, OBJECT_LIST, Some(1)).unwrap(),
            [0xC4, 0x02, 0x00, 0x00, 10]
        );
        assert_eq!(
            value(&db, &d, Target::Device, OBJECT_LIST, None)
                .unwrap()
                .len(),
            8 * 5
        );
        // property-list leaves out object-identifier, -name, -type and itself
        let list = property_list(&d, av);
        assert!(
            list.contains(&PRESENT_VALUE)
                && list.contains(&MIN_PRES_VALUE)
                && !list.contains(&OBJECT_NAME)
        );
        assert!(!list.contains(&MAX_PRES_VALUE) && !list.contains(&PROPERTY_LIST));

        // The new arrays: one day of the week, event time stamps unset.
        let sched = Target::Object(4);
        assert_eq!(
            value(&db, &d, sched, WEEKLY_SCHEDULE, Some(0)).unwrap(),
            [0x21, 7]
        );
        assert_eq!(
            value(&db, &d, sched, WEEKLY_SCHEDULE, Some(3)).unwrap(),
            [0x0E, 0xB4, 7, 0, 0, 0, 0x91, 1, 0x0F]
        );
        assert_eq!(
            value(&db, &d, sched, EXCEPTION_SCHEDULE, Some(0)).unwrap(),
            [0x21, 0]
        );
        assert_eq!(
            value(&db, &d, Target::Object(3), EVENT_TIME_STAMPS, Some(1)).unwrap(),
            [
                0x2E, 0xA4, 255, 255, 255, 255, 0xB4, 255, 255, 255, 255, 0x2F
            ]
        );
        assert_eq!(
            value(&db, &d, Target::Object(6), PRIORITY, None).unwrap(),
            [0x21, 100, 0x21, 100, 0x21, 200]
        );
    }

    #[test]
    fn alarm_properties_only_with_reporting() {
        let (_, d) = device();
        assert!(!has_property(&d, Target::Object(0), EVENT_ENABLE));
        assert!(has_property(&d, Target::Object(2), ALARM_VALUE));
        assert!(!has_property(&d, Target::Object(2), HIGH_LIMIT));
        assert!(has_property(&d, Target::Object(3), HIGH_LIMIT));
        assert!(has_property(&d, Target::Object(3), EVENT_TIME_STAMPS));
        assert!(!has_property(&d, Target::Object(5), PRESENT_VALUE));
        assert!(!has_property(&d, Target::Object(6), STATUS_FLAGS));
    }

    #[test]
    fn local_time_follows_the_sim() {
        let (mut db, d) = device();
        db.now = NaiveDate::from_ymd_opt(2026, 7, 4)
            .unwrap()
            .and_hms_opt(13, 30, 5);
        assert_eq!(
            value(&db, &d, Target::Device, LOCAL_DATE, None).unwrap(),
            [0xA4, 126, 7, 4, 6]
        );
        assert_eq!(
            value(&db, &d, Target::Device, LOCAL_TIME, None).unwrap(),
            [0xB4, 13, 30, 5, 0]
        );
    }

    #[test]
    fn wildcard_and_unknown_objects() {
        let (_, d) = device();
        assert_eq!(
            resolve(&d, ObjectId::new(8, ObjectId::WILDCARD)),
            Ok(Target::Device)
        );
        assert_eq!(resolve(&d, ObjectId::new(8, 10)), Ok(Target::Device));
        assert_eq!(
            resolve(&d, ObjectId::new(8, 11)),
            Err(BacError::UNKNOWN_OBJECT)
        );
        assert_eq!(resolve(&d, ObjectId::new(19, 2)), Ok(Target::Object(1)));
        assert_eq!(
            resolve(&d, ObjectId::new(0, 2)),
            Err(BacError::UNKNOWN_OBJECT)
        );
        assert_eq!(
            resolve(&d, ObjectId::new(10, 1)),
            Err(BacError::UNKNOWN_OBJECT)
        );
        assert_eq!(resolve(&d, ObjectId::new(17, 1)), Ok(Target::Object(4)));
    }

    #[test]
    fn services_supported_bits() {
        let (db, d) = device();
        let v = value(&db, &d, Target::Device, PROTOCOL_SERVICES_SUPPORTED, None).unwrap();
        // 41 bits: 6 octets, 7 unused
        assert_eq!(&v[..3], [0x85, 0x07, 7]);
        let bit = |n: usize| v[3 + n / 8] & (0x80 >> (n % 8)) != 0;
        for n in [0, 2, 5, 12, 14, 15, 16, 26, 27, 29, 33, 34, 35, 39] {
            assert!(bit(n), "service bit {n}");
        }
        assert!(!bit(3) && !bit(6) && !bit(32) && !bit(36));
        let v = value(
            &db,
            &d,
            Target::Device,
            PROTOCOL_OBJECT_TYPES_SUPPORTED,
            None,
        )
        .unwrap();
        let bit = |n: usize| v[3 + n / 8] & (0x80 >> (n % 8)) != 0;
        for n in [0, 1, 2, 3, 4, 5, 8, 13, 14, 15, 17, 19, 20] {
            assert!(bit(n), "object type bit {n}");
        }
        assert!(!bit(6) && !bit(10));
    }
}
