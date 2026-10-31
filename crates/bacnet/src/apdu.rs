//! APDU headers (clause 20.1) and the replies a server builds from them.

use crate::encoding::{app_enumerated, close, open};

/// Confirmed service choices.
pub mod confirmed {
    pub const COV_NOTIFICATION: u8 = 1;
    pub const SUBSCRIBE_COV: u8 = 5;
    pub const READ_PROPERTY: u8 = 12;
    pub const READ_PROPERTY_MULTIPLE: u8 = 14;
    pub const WRITE_PROPERTY: u8 = 15;
    pub const WRITE_PROPERTY_MULTIPLE: u8 = 16;
    pub const DEVICE_COMMUNICATION_CONTROL: u8 = 17;
    pub const REINITIALIZE_DEVICE: u8 = 20;
}

/// Unconfirmed service choices.
pub mod unconfirmed {
    pub const I_AM: u8 = 0;
    pub const I_HAVE: u8 = 1;
    pub const COV_NOTIFICATION: u8 = 2;
    pub const WHO_HAS: u8 = 7;
    pub const WHO_IS: u8 = 8;
}

pub const REJECT_UNRECOGNIZED_SERVICE: u8 = 9;
pub const ABORT_SEGMENTATION_NOT_SUPPORTED: u8 = 4;

/// The largest APDU any of our devices sends or accepts.
pub const MAX_APDU: usize = 1476;

/// An error class and code, as carried in an Error PDU or a property access
/// error inside a ReadPropertyMultiple-ACK.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BacError {
    pub class: u32,
    pub code: u32,
}

impl BacError {
    const fn new(class: u32, code: u32) -> Self {
        BacError { class, code }
    }

    pub const UNKNOWN_OBJECT: Self = Self::new(1, 31);
    /// SubscribeCOV on an object that doesn't report changes (the device).
    pub const OPTIONAL_FUNCTIONALITY_NOT_SUPPORTED: Self = Self::new(1, 45);
    pub const INVALID_DATA_TYPE: Self = Self::new(2, 9);
    pub const READ_ACCESS_DENIED: Self = Self::new(2, 27);
    pub const UNKNOWN_PROPERTY: Self = Self::new(2, 32);
    pub const VALUE_OUT_OF_RANGE: Self = Self::new(2, 37);
    pub const WRITE_ACCESS_DENIED: Self = Self::new(2, 40);
    pub const INVALID_ARRAY_INDEX: Self = Self::new(2, 42);
    pub const NOT_AN_ARRAY: Self = Self::new(2, 50);
    pub const NO_SPACE: Self = Self::new(3, 19);

    pub fn encode(self, buf: &mut Vec<u8>) {
        app_enumerated(buf, self.class);
        app_enumerated(buf, self.code);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfirmedHeader {
    /// The request itself arrived in segments, which we don't reassemble.
    pub segmented: bool,
    /// The largest APDU the client accepts.
    pub max_apdu: usize,
    pub invoke_id: u8,
    pub service: u8,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Apdu<'a> {
    Confirmed(ConfirmedHeader, &'a [u8]),
    Unconfirmed(u8, &'a [u8]),
    /// Acks, errors, rejects and aborts: answers to our own confirmed COV
    /// notifications, which we don't track.
    Other,
}

pub fn parse_apdu(b: &[u8]) -> Option<Apdu<'_>> {
    let first = *b.first()?;
    Some(match first >> 4 {
        0 => {
            // type/flags, max-segments + max-APDU, invoke id,
            // [sequence number, proposed window if segmented], service
            let segmented = first & 0x08 != 0;
            let skip = if segmented { 2 } else { 0 };
            let service = *b.get(3 + skip)?;
            let header = ConfirmedHeader {
                segmented,
                max_apdu: max_apdu_len(b[1] & 0x0F),
                invoke_id: b[2],
                service,
            };
            Apdu::Confirmed(header, &b[4 + skip..])
        }
        1 => Apdu::Unconfirmed(*b.get(1)?, &b[2..]),
        _ => Apdu::Other,
    })
}

/// The max-APDU-length-accepted code in a confirmed request header.
/// Max-segments doesn't matter: we never segment.
fn max_apdu_len(bits: u8) -> usize {
    match bits {
        0 => 50,
        1 => 128,
        2 => 206,
        3 => 480,
        4 => 1024,
        _ => MAX_APDU,
    }
}

pub fn simple_ack(invoke_id: u8, service: u8) -> Vec<u8> {
    vec![0x20, invoke_id, service]
}

pub fn complex_ack(invoke_id: u8, service: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![0x30, invoke_id, service];
    v.extend_from_slice(body);
    v
}

pub fn error(invoke_id: u8, service: u8, err: BacError) -> Vec<u8> {
    let mut v = vec![0x50, invoke_id, service];
    err.encode(&mut v);
    v
}

/// WritePropertyMultiple's error, which wraps the error and appends a
/// reference to the first write that failed.
pub fn wpm_error(invoke_id: u8, err: BacError, failed_write: &[u8]) -> Vec<u8> {
    let mut v = vec![0x50, invoke_id, confirmed::WRITE_PROPERTY_MULTIPLE];
    open(&mut v, 0);
    err.encode(&mut v);
    close(&mut v, 0);
    open(&mut v, 1);
    v.extend_from_slice(failed_write);
    close(&mut v, 1);
    v
}

pub fn reject(invoke_id: u8, reason: u8) -> Vec<u8> {
    vec![0x60, invoke_id, reason]
}

/// An Abort sent by the server.
pub fn abort(invoke_id: u8, reason: u8) -> Vec<u8> {
    vec![0x71, invoke_id, reason]
}

pub fn unconfirmed_request(service: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![0x10, service];
    v.extend_from_slice(body);
    v
}

/// A confirmed request from us (COV notifications): unsegmented, and we
/// accept replies up to 1476 bytes.
pub fn confirmed_request(invoke_id: u8, service: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![0x00, 0x05, invoke_id, service];
    v.extend_from_slice(body);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmed_header() {
        // segmented-response-accepted, up to 4 segments, max APDU 1476
        let b = [0x02, 0x25, 0x07, 0x0C, 0xAA];
        let Some(Apdu::Confirmed(h, body)) = parse_apdu(&b) else {
            panic!()
        };
        assert_eq!(
            h,
            ConfirmedHeader {
                segmented: false,
                max_apdu: 1476,
                invoke_id: 7,
                service: 12
            }
        );
        assert_eq!(body, [0xAA]);
        // max APDU 50, segmented request with sequence number and window
        let b = [0x0C, 0x00, 0x09, 0x00, 0x04, 0x0E, 0xBB];
        let Some(Apdu::Confirmed(h, body)) = parse_apdu(&b) else {
            panic!()
        };
        assert_eq!(
            h,
            ConfirmedHeader {
                segmented: true,
                max_apdu: 50,
                invoke_id: 9,
                service: 14
            }
        );
        assert_eq!(body, [0xBB]);
        assert_eq!(parse_apdu(&[0x00, 0x05, 0x01]), None);
    }

    #[test]
    fn unconfirmed_and_others() {
        assert_eq!(
            parse_apdu(&[0x10, 0x08]),
            Some(Apdu::Unconfirmed(8, &[][..]))
        );
        assert_eq!(parse_apdu(&[0x20, 0x01, 0x01]), Some(Apdu::Other));
        assert_eq!(parse_apdu(&[]), None);
    }

    #[test]
    fn error_pdus() {
        assert_eq!(
            error(3, 12, BacError::UNKNOWN_OBJECT),
            [0x50, 3, 12, 0x91, 1, 0x91, 31]
        );
        let e = wpm_error(4, BacError::WRITE_ACCESS_DENIED, &[0x09, 0x55]);
        assert_eq!(
            e,
            [
                0x50, 4, 16, 0x0E, 0x91, 2, 0x91, 40, 0x0F, 0x1E, 0x09, 0x55, 0x1F
            ]
        );
    }
}
