//! BACnet/IP framing (Annex J BVLC) and the network layer (clause 6 NPDU).
//!
//! The NPDU is where routing lives. DNET/DADR name the remote network and
//! station a message is for; SNET/SADR name where a routed message came from.
//! The site controller is a router between BACnet/IP and the simulator's
//! virtual network, so traffic to and from every other device carries one or
//! the other, exactly as it would through a real IP-to-MS/TP router.

use std::net::{Ipv4Addr, SocketAddrV4};

const BVLC_TYPE: u8 = 0x81;
const BVLC_RESULT: u8 = 0x00;
const WRITE_BDT: u8 = 0x01;
const READ_BDT: u8 = 0x02;
const FORWARDED_NPDU: u8 = 0x04;
const REGISTER_FOREIGN_DEVICE: u8 = 0x05;
const READ_FDT: u8 = 0x06;
const DELETE_FDT_ENTRY: u8 = 0x08;
const DISTRIBUTE_BROADCAST: u8 = 0x09;
pub const ORIGINAL_UNICAST: u8 = 0x0A;
pub const ORIGINAL_BROADCAST: u8 = 0x0B;

/// Network-layer message types.
pub const WHO_IS_ROUTER_TO_NETWORK: u8 = 0x00;
pub const I_AM_ROUTER_TO_NETWORK: u8 = 0x01;
pub const REJECT_MESSAGE_TO_NETWORK: u8 = 0x03;

/// DNET for a global broadcast.
pub const GLOBAL_NETWORK: u16 = 0xFFFF;

/// What a received BVLC frame holds.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame<'a> {
    /// An NPDU. `origin` is the original sender of a Forwarded-NPDU.
    Npdu {
        npdu: &'a [u8],
        broadcast: bool,
        origin: Option<SocketAddrV4>,
    },
    /// A BBMD or foreign-device request we don't serve: answer with this
    /// BVLC-Result NAK code.
    Nak(u16),
}

pub fn parse_bvlc(p: &[u8]) -> Option<Frame<'_>> {
    if p.len() < 4 || p[0] != BVLC_TYPE {
        return None;
    }
    let len = u16::from_be_bytes([p[2], p[3]]) as usize;
    if len < 4 || len > p.len() {
        return None;
    }
    let p = &p[..len];
    Some(match p[1] {
        ORIGINAL_UNICAST => Frame::Npdu {
            npdu: &p[4..],
            broadcast: false,
            origin: None,
        },
        ORIGINAL_BROADCAST => Frame::Npdu {
            npdu: &p[4..],
            broadcast: true,
            origin: None,
        },
        FORWARDED_NPDU if len >= 10 => {
            let ip = Ipv4Addr::new(p[4], p[5], p[6], p[7]);
            let port = u16::from_be_bytes([p[8], p[9]]);
            Frame::Npdu {
                npdu: &p[10..],
                broadcast: true,
                origin: Some(SocketAddrV4::new(ip, port)),
            }
        }
        WRITE_BDT => Frame::Nak(0x0010),
        READ_BDT => Frame::Nak(0x0020),
        REGISTER_FOREIGN_DEVICE => Frame::Nak(0x0030),
        READ_FDT => Frame::Nak(0x0040),
        DELETE_FDT_ENTRY => Frame::Nak(0x0050),
        DISTRIBUTE_BROADCAST => Frame::Nak(0x0060),
        // BVLC-Result, BDT/FDT acks, Secure-BVLL: nothing for us to do.
        _ => return None,
    })
}

pub fn bvlc(function: u8, npdu: &[u8]) -> Vec<u8> {
    let len = (npdu.len() + 4) as u16;
    let mut p = vec![BVLC_TYPE, function];
    p.extend_from_slice(&len.to_be_bytes());
    p.extend_from_slice(npdu);
    p
}

pub fn bvlc_result(code: u16) -> Vec<u8> {
    let [hi, lo] = code.to_be_bytes();
    vec![BVLC_TYPE, BVLC_RESULT, 0x00, 0x06, hi, lo]
}

/// A station on a BACnet network; an empty `mac` is a broadcast on `net`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NetAddr {
    pub net: u16,
    pub mac: Vec<u8>,
}

/// Where a request came from, and so where its replies go.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Peer {
    /// The B/IP address to answer: the client, or the router it came through.
    pub addr: SocketAddrV4,
    /// The client's own network address when it sits behind another router.
    pub remote: Option<NetAddr>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Npdu {
    pub dest: Option<NetAddr>,
    pub source: Option<NetAddr>,
    pub expecting_reply: bool,
    /// Set for network-layer messages.
    pub message: Option<u8>,
}

/// Splits an NPDU into its header and payload (APDU or message data).
pub fn parse_npdu(b: &[u8]) -> Option<(Npdu, &[u8])> {
    if b.len() < 2 || b[0] != 0x01 {
        return None;
    }
    let ctrl = b[1];
    let mut i = 2;
    let addr = |i: &mut usize| -> Option<NetAddr> {
        let net = u16::from_be_bytes([*b.get(*i)?, *b.get(*i + 1)?]);
        let len = *b.get(*i + 2)? as usize;
        let mac = b.get(*i + 3..*i + 3 + len)?.to_vec();
        *i += 3 + len;
        Some(NetAddr { net, mac })
    };
    let dest = if ctrl & 0x20 != 0 {
        Some(addr(&mut i)?)
    } else {
        None
    };
    let source = if ctrl & 0x08 != 0 {
        Some(addr(&mut i)?)
    } else {
        None
    };
    if dest.is_some() {
        i += 1; // hop count
    }
    let message = if ctrl & 0x80 != 0 {
        let m = *b.get(i)?;
        i += if m >= 0x80 { 3 } else { 1 }; // proprietary types carry a vendor id
        Some(m)
    } else {
        None
    };
    let payload = b.get(i..)?;
    Some((
        Npdu {
            dest,
            source,
            expecting_reply: ctrl & 0x04 != 0,
            message,
        },
        payload,
    ))
}

pub fn build_npdu(npdu: &Npdu, payload: &[u8]) -> Vec<u8> {
    let mut ctrl = 0u8;
    if npdu.message.is_some() {
        ctrl |= 0x80;
    }
    if npdu.dest.is_some() {
        ctrl |= 0x20;
    }
    if npdu.source.is_some() {
        ctrl |= 0x08;
    }
    if npdu.expecting_reply {
        ctrl |= 0x04;
    }
    let mut v = vec![0x01, ctrl];
    for a in [&npdu.dest, &npdu.source].into_iter().flatten() {
        v.extend_from_slice(&a.net.to_be_bytes());
        v.push(a.mac.len() as u8);
        v.extend_from_slice(&a.mac);
    }
    if npdu.dest.is_some() {
        v.push(255); // hop count
    }
    if let Some(m) = npdu.message {
        v.push(m);
    }
    v.extend_from_slice(payload);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routed_npdu_round_trip() {
        let n = Npdu {
            dest: Some(NetAddr {
                net: 7,
                mac: vec![10, 0, 0, 9, 0xBA, 0xC0],
            }),
            source: Some(NetAddr {
                net: 2001,
                mac: vec![5],
            }),
            expecting_reply: true,
            message: None,
        };
        let b = build_npdu(&n, &[0xAA, 0xBB]);
        assert_eq!(&b[..2], [0x01, 0x2C]);
        assert_eq!(&b[2..5], [0x00, 0x07, 6]);
        assert_eq!(&b[11..15], [0x07, 0xD1, 1, 5]);
        assert_eq!(b[15], 255);
        let (back, payload) = parse_npdu(&b).unwrap();
        assert_eq!(back, n);
        assert_eq!(payload, [0xAA, 0xBB]);
    }

    #[test]
    fn local_npdu_is_two_bytes() {
        let b = build_npdu(&Npdu::default(), &[0x10, 0x08]);
        assert_eq!(b, [0x01, 0x00, 0x10, 0x08]);
        let (n, payload) = parse_npdu(&b).unwrap();
        assert_eq!(n, Npdu::default());
        assert_eq!(payload, [0x10, 0x08]);
    }

    #[test]
    fn network_messages() {
        // Who-Is-Router-To-Network, global broadcast, asking about 2001
        let b = [0x01, 0xA0, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0x07, 0xD1];
        let (n, data) = parse_npdu(&b).unwrap();
        assert_eq!(n.message, Some(WHO_IS_ROUTER_TO_NETWORK));
        assert_eq!(
            n.dest,
            Some(NetAddr {
                net: GLOBAL_NETWORK,
                mac: vec![]
            })
        );
        assert_eq!(data, [0x07, 0xD1]);
        let reply = build_npdu(
            &Npdu {
                message: Some(I_AM_ROUTER_TO_NETWORK),
                ..Default::default()
            },
            &2001u16.to_be_bytes(),
        );
        assert_eq!(reply, [0x01, 0x80, 0x01, 0x07, 0xD1]);
        // A proprietary message skips its vendor id.
        let (n, data) = parse_npdu(&[0x01, 0x80, 0x80, 0x01, 0x04, 0x42]).unwrap();
        assert_eq!((n.message, data), (Some(0x80), &[0x42][..]));
    }

    #[test]
    fn truncated_npdus_fail() {
        assert!(parse_npdu(&[0x01]).is_none());
        assert!(parse_npdu(&[0x02, 0x00]).is_none());
        assert!(parse_npdu(&[0x01, 0x20, 0x07, 0xD1, 0x03, 1]).is_none());
    }

    #[test]
    fn bvlc_frames() {
        let p = bvlc(ORIGINAL_UNICAST, &[0x01, 0x00]);
        assert_eq!(p, [0x81, 0x0A, 0x00, 0x06, 0x01, 0x00]);
        assert_eq!(
            parse_bvlc(&p),
            Some(Frame::Npdu {
                npdu: &[0x01, 0x00],
                broadcast: false,
                origin: None
            })
        );
        let fwd = [
            0x81, 0x04, 0x00, 0x0C, 192, 168, 1, 20, 0xBA, 0xC0, 0x01, 0x00,
        ];
        assert_eq!(
            parse_bvlc(&fwd),
            Some(Frame::Npdu {
                npdu: &[0x01, 0x00],
                broadcast: true,
                origin: Some(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 20), 47808)),
            })
        );
        assert_eq!(
            parse_bvlc(&[0x81, 0x05, 0x00, 0x06, 0x00, 0x3C]),
            Some(Frame::Nak(0x0030))
        );
        assert_eq!(
            parse_bvlc(&[0x81, 0x02, 0x00, 0x04]),
            Some(Frame::Nak(0x0020))
        );
        assert_eq!(
            parse_bvlc(&[0x81, 0x06, 0x00, 0x04]),
            Some(Frame::Nak(0x0040))
        );
        assert_eq!(parse_bvlc(&bvlc_result(0x0030)), None);
        assert_eq!(parse_bvlc(&[0x81, 0x0A, 0x00, 0x09, 0x01]), None);
    }
}
