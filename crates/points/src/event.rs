//! Intrinsic reporting (clause 13.3) and notification classes (12.21).
//!
//! A point with reporting turned on runs its event algorithm every tick:
//! CHANGE_OF_STATE for binaries, OUT_OF_RANGE for analogs, and a fault check
//! on Reliability for both. A transition to off-normal or back to normal has
//! to hold for Time_Delay of sim time; fault transitions happen at once. Each
//! transition stamps Event_Time_Stamps, clears its Acked_Transitions bit if
//! the notification class wants acknowledgment, and queues an
//! [`EventNotice`] for the BACnet server to send to the class's recipients.

use crate::{BacObject, PointValue, Reliability, format_value};
use chrono::{Datelike, Duration, NaiveDateTime, NaiveTime, Timelike};

/// BACnetEventState values the simulator uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventState {
    Normal,
    Fault,
    Offnormal,
    HighLimit,
    LowLimit,
}

impl EventState {
    pub fn bacnet_id(self) -> u32 {
        match self {
            EventState::Normal => 0,
            EventState::Fault => 1,
            EventState::Offnormal => 2,
            EventState::HighLimit => 3,
            EventState::LowLimit => 4,
        }
    }

    pub fn from_bacnet_id(id: u32) -> Option<Self> {
        Some(match id {
            0 => EventState::Normal,
            1 => EventState::Fault,
            2 => EventState::Offnormal,
            3 => EventState::HighLimit,
            4 => EventState::LowLimit,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            EventState::Normal => "normal",
            EventState::Fault => "fault",
            EventState::Offnormal => "alarm",
            EventState::HighLimit => "high limit",
            EventState::LowLimit => "low limit",
        }
    }
}

/// The three kinds of transition, in BACnetEventTransitionBits order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transition {
    ToOffnormal,
    ToFault,
    ToNormal,
}

impl Transition {
    /// The transition that ends in `state`.
    pub fn to(state: EventState) -> Self {
        match state {
            EventState::Normal => Transition::ToNormal,
            EventState::Fault => Transition::ToFault,
            _ => Transition::ToOffnormal,
        }
    }

    /// Its bit in Event_Enable, Acked_Transitions and the rest.
    pub fn index(self) -> usize {
        self as usize
    }
}

/// BACnetEventType values the simulator reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventType {
    ChangeOfState,
    OutOfRange,
    ChangeOfReliability,
}

impl EventType {
    pub fn bacnet_id(self) -> u32 {
        match self {
            EventType::ChangeOfState => 1,
            EventType::OutOfRange => 5,
            EventType::ChangeOfReliability => 19,
        }
    }
}

/// BACnetNotifyType.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyType {
    Alarm,
    Event,
    AckNotification,
}

impl NotifyType {
    pub fn bacnet_id(self) -> u32 {
        self as u32
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EventAlgorithm {
    /// Off-normal while the present value equals Alarm_Value.
    ChangeOfState { alarm_value: bool },
    /// High limit above High_Limit, low limit below Low_Limit; back to
    /// normal once inside by Deadband. Limit_Enable switches each side.
    OutOfRange {
        low: f32,
        high: f32,
        deadband: f32,
        low_enable: bool,
        high_enable: bool,
    },
}

impl EventAlgorithm {
    pub fn event_type(&self) -> EventType {
        match self {
            EventAlgorithm::ChangeOfState { .. } => EventType::ChangeOfState,
            EventAlgorithm::OutOfRange { .. } => EventType::OutOfRange,
        }
    }

    /// The state the value calls for, coming from `state`.
    fn target(&self, state: EventState, value: PointValue) -> EventState {
        match *self {
            EventAlgorithm::ChangeOfState { alarm_value } => {
                if value.as_bool() == alarm_value {
                    EventState::Offnormal
                } else {
                    EventState::Normal
                }
            }
            EventAlgorithm::OutOfRange {
                low,
                high,
                deadband,
                low_enable,
                high_enable,
            } => {
                let v = value.as_f64() as f32;
                let (above, below) = (high_enable && v > high, low_enable && v < low);
                match state {
                    EventState::HighLimit if below => EventState::LowLimit,
                    EventState::HighLimit if high_enable && v >= high - deadband => state,
                    EventState::LowLimit if above => EventState::HighLimit,
                    EventState::LowLimit if low_enable && v <= low + deadband => state,
                    _ if above => EventState::HighLimit,
                    _ if below => EventState::LowLimit,
                    _ => EventState::Normal,
                }
            }
        }
    }
}

/// A point's intrinsic reporting: its configuration and where it stands.
#[derive(Clone, Debug)]
pub struct EventReporting {
    pub algorithm: EventAlgorithm,
    pub notification_class: u32,
    /// Time_Delay, seconds of sim time.
    pub time_delay: u32,
    /// Event_Enable: to-offnormal, to-fault, to-normal.
    pub event_enable: [bool; 3],
    pub notify_type: NotifyType,
    /// Event_Detection_Enable. Off holds the state at normal.
    pub detection_enable: bool,
    pub state: EventState,
    /// Acked_Transitions: to-offnormal, to-fault, to-normal.
    pub acked: [bool; 3],
    /// Event_Time_Stamps: when each transition last happened.
    pub time_stamps: [Option<NaiveDateTime>; 3],
    /// The event type each transition was reported as, for its
    /// acknowledgment notification.
    event_types: [EventType; 3],
    /// A transition waiting out the time delay: to where, since when.
    pending: Option<(EventState, NaiveDateTime)>,
}

impl EventReporting {
    pub fn new(algorithm: EventAlgorithm, notification_class: u32, time_delay: u32) -> Self {
        EventReporting {
            algorithm,
            notification_class,
            time_delay,
            event_enable: [true; 3],
            notify_type: NotifyType::Alarm,
            detection_enable: true,
            state: EventState::Normal,
            acked: [true; 3],
            time_stamps: [None; 3],
            event_types: [algorithm.event_type(); 3],
            pending: None,
        }
    }

    /// A transition nobody has acknowledged yet.
    pub fn unacknowledged(&self) -> bool {
        self.acked.contains(&false)
    }

    /// Event_Detection_Enable. Turning it off returns the object to normal
    /// with nothing left to acknowledge, as the standard has it.
    pub fn set_detection_enable(&mut self, enable: bool) {
        self.detection_enable = enable;
        if !enable {
            self.state = EventState::Normal;
            self.acked = [true; 3];
            self.pending = None;
        }
    }

    /// One tick of the algorithm. Returns the transition made, as (from, to).
    pub(crate) fn evaluate(
        &mut self,
        value: PointValue,
        fault: bool,
        now: NaiveDateTime,
    ) -> Option<(EventState, EventState)> {
        if !self.detection_enable {
            return None;
        }
        let from = self.state;
        let to = if fault {
            EventState::Fault
        } else if from == EventState::Fault {
            EventState::Normal
        } else {
            self.algorithm.target(from, value)
        };
        if to == from {
            self.pending = None;
            return None;
        }
        // Into and out of fault at once; anything else waits out the delay.
        let reliability = to == EventState::Fault || from == EventState::Fault;
        if !reliability {
            let since = match self.pending {
                Some((s, since)) if s == to => since,
                _ => {
                    self.pending = Some((to, now));
                    now
                }
            };
            if now - since < Duration::seconds(self.time_delay as i64) {
                return None;
            }
        }
        self.pending = None;
        self.state = to;
        let t = Transition::to(to).index();
        self.time_stamps[t] = Some(now);
        self.event_types[t] = if reliability {
            EventType::ChangeOfReliability
        } else {
            self.algorithm.event_type()
        };
        Some((from, to))
    }

    pub(crate) fn event_type(&self, t: Transition) -> EventType {
        self.event_types[t.index()]
    }
}

/// Where a notification class sends its notifications (BACnetRecipient).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recipient {
    /// A device, found with Who-Is.
    Device(u32),
    /// Network 0 is the local network; an empty MAC broadcasts on the
    /// network, and network 65535 is a global broadcast.
    Address { network: u16, mac: Vec<u8> },
}

/// One entry of Recipient_List (BACnetDestination).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    /// Monday first.
    pub days: [bool; 7],
    pub from: NaiveTime,
    pub to: NaiveTime,
    pub recipient: Recipient,
    pub process_id: u32,
    pub confirmed: bool,
    /// to-offnormal, to-fault, to-normal.
    pub transitions: [bool; 3],
}

impl Destination {
    /// Every day and time, every transition, unconfirmed to everyone on the
    /// local network: alarms reach any front end with no setup.
    pub fn local_broadcast() -> Self {
        Destination {
            days: [true; 7],
            from: NaiveTime::MIN,
            to: NaiveTime::from_hms_milli_opt(23, 59, 59, 990).unwrap(),
            recipient: Recipient::Address {
                network: 0,
                mac: Vec::new(),
            },
            process_id: 0,
            confirmed: false,
            transitions: [true; 3],
        }
    }

    /// Whether a notification of `transition` stamped `at` goes here.
    pub fn accepts(&self, at: NaiveDateTime, transition: Transition) -> bool {
        // BACnet times stop at hundredths.
        let time = at.time();
        let time = time
            .with_nanosecond(time.nanosecond() / 10_000_000 * 10_000_000)
            .unwrap_or(time);
        self.transitions[transition.index()]
            && self.days[at.weekday().num_days_from_monday() as usize]
            && self.from <= time
            && time <= self.to
    }
}

/// A notification class object's data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationClass {
    /// Priority: to-offnormal, to-fault, to-normal.
    pub priority: [u8; 3],
    pub ack_required: [bool; 3],
    pub recipients: Vec<Destination>,
}

impl Default for NotificationClass {
    fn default() -> Self {
        NotificationClass {
            priority: [100, 100, 200],
            ack_required: [true, true, false],
            recipients: vec![Destination::local_broadcast()],
        }
    }
}

/// The event values a notification carries (BACnetNotificationParameters).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EventValues {
    ChangeOfState {
        value: bool,
        flags: [bool; 4],
    },
    OutOfRange {
        value: f32,
        flags: [bool; 4],
        deadband: f32,
        limit: f32,
    },
    ChangeOfReliability {
        reliability: Reliability,
        flags: [bool; 4],
        value: PointValue,
    },
}

/// An event notification waiting for the BACnet server, which sends it to
/// the recipients its notification class lists at that moment.
#[derive(Clone, Debug, PartialEq)]
pub struct EventNotice {
    /// The initiating device's instance.
    pub device: u32,
    pub object_type: crate::ObjectType,
    pub instance: u32,
    /// The transition's time stamp, which an acknowledgment must quote.
    pub time: NaiveDateTime,
    pub notification_class: u32,
    pub priority: u8,
    pub event_type: EventType,
    pub notify_type: NotifyType,
    pub ack_required: bool,
    pub from: EventState,
    pub to: EventState,
    /// None for acknowledgment notifications.
    pub values: Option<EventValues>,
    pub text: String,
}

impl EventNotice {
    pub fn transition(&self) -> Transition {
        Transition::to(self.to)
    }
}

/// The notification for `o`'s transition from `from` to `to`.
pub(crate) fn notice(
    device: u32,
    o: &BacObject,
    (from, to): (EventState, EventState),
    now: NaiveDateTime,
    (priority, ack_required): (u8, bool),
) -> Option<EventNotice> {
    let r = o.reporting.as_deref()?;
    let flags = o.status_flags();
    let value = o.effective();
    let (event_type, values) = if to == EventState::Fault || from == EventState::Fault {
        (
            EventType::ChangeOfReliability,
            EventValues::ChangeOfReliability {
                reliability: o.reliability,
                flags,
                value,
            },
        )
    } else {
        let values = match r.algorithm {
            EventAlgorithm::ChangeOfState { .. } => EventValues::ChangeOfState {
                value: value.as_bool(),
                flags,
            },
            EventAlgorithm::OutOfRange {
                low,
                high,
                deadband,
                ..
            } => EventValues::OutOfRange {
                value: value.as_f64() as f32,
                flags,
                deadband,
                limit: if to == EventState::LowLimit || from == EventState::LowLimit {
                    low
                } else {
                    high
                },
            },
        };
        (r.algorithm.event_type(), values)
    };
    let text = if to == EventState::Fault {
        format!("{} fault: {}", o.name, o.reliability.label())
    } else {
        format!("{} {}: {}", o.name, to.label(), format_value(o, value))
    };
    Some(EventNotice {
        device,
        object_type: o.object_type,
        instance: o.instance,
        time: now,
        notification_class: r.notification_class,
        priority,
        event_type,
        notify_type: r.notify_type,
        ack_required,
        from,
        to,
        values: Some(values),
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn t(s: i64) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 2)
            .unwrap()
            .and_hms_opt(8, 0, 0)
            .unwrap()
            + Duration::seconds(s)
    }

    #[test]
    fn out_of_range_with_time_delay_and_deadband() {
        let mut r = EventReporting::new(
            EventAlgorithm::OutOfRange {
                low: 60.0,
                high: 80.0,
                deadband: 2.0,
                low_enable: true,
                high_enable: true,
            },
            1,
            60,
        );
        let v = PointValue::Real;
        assert_eq!(r.evaluate(v(81.0), false, t(0)), None);
        assert_eq!(r.evaluate(v(81.0), false, t(59)), None);
        // A dip below the limit restarts the delay.
        assert_eq!(r.evaluate(v(79.0), false, t(60)), None);
        assert_eq!(r.evaluate(v(81.0), false, t(61)), None);
        assert_eq!(r.evaluate(v(81.0), false, t(120)), None);
        assert_eq!(
            r.evaluate(v(81.0), false, t(121)),
            Some((EventState::Normal, EventState::HighLimit))
        );
        assert_eq!(r.time_stamps[0], Some(t(121)));
        // Inside the deadband it stays in alarm.
        assert_eq!(r.evaluate(v(78.5), false, t(200)), None);
        assert_eq!(r.evaluate(v(78.5), false, t(400)), None);
        assert_eq!(r.state, EventState::HighLimit);
        assert_eq!(r.evaluate(v(77.0), false, t(401)), None);
        assert_eq!(
            r.evaluate(v(77.0), false, t(461)),
            Some((EventState::HighLimit, EventState::Normal))
        );
        // Straight from high to low.
        r.state = EventState::HighLimit;
        r.time_delay = 0;
        assert_eq!(
            r.evaluate(v(50.0), false, t(500)),
            Some((EventState::HighLimit, EventState::LowLimit))
        );
    }

    #[test]
    fn faults_are_immediate_and_return_to_normal() {
        let mut r = EventReporting::new(EventAlgorithm::ChangeOfState { alarm_value: true }, 1, 30);
        let on = PointValue::Binary(true);
        assert_eq!(r.evaluate(on, false, t(0)), None);
        // Fault beats the pending alarm, with no delay.
        assert_eq!(
            r.evaluate(on, true, t(10)),
            Some((EventState::Normal, EventState::Fault))
        );
        assert_eq!(
            r.event_type(Transition::ToFault),
            EventType::ChangeOfReliability
        );
        // When it clears it goes to normal at once, then the alarm waits out
        // its own delay.
        assert_eq!(
            r.evaluate(on, false, t(20)),
            Some((EventState::Fault, EventState::Normal))
        );
        assert_eq!(r.evaluate(on, false, t(21)), None);
        assert_eq!(
            r.evaluate(on, false, t(51)),
            Some((EventState::Normal, EventState::Offnormal))
        );
        assert_eq!(
            r.event_type(Transition::ToOffnormal),
            EventType::ChangeOfState
        );

        r.set_detection_enable(false);
        assert_eq!(r.state, EventState::Normal);
        assert_eq!(r.evaluate(on, true, t(60)), None);
    }

    #[test]
    fn destinations_filter_by_day_time_and_transition() {
        let mut d = Destination::local_broadcast();
        let late = NaiveDate::from_ymd_opt(2026, 11, 2)
            .unwrap()
            .and_hms_milli_opt(23, 59, 59, 995)
            .unwrap();
        assert!(d.accepts(late, Transition::ToNormal));
        d.days[0] = false; // not Mondays
        assert!(!d.accepts(t(0), Transition::ToOffnormal));
        d.days[0] = true;
        d.from = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        assert!(!d.accepts(t(0), Transition::ToOffnormal));
        assert!(d.accepts(t(3600), Transition::ToOffnormal));
        d.transitions[2] = false;
        assert!(!d.accepts(t(3600), Transition::ToNormal));
    }
}
