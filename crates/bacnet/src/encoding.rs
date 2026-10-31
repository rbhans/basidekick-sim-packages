//! BACnet's tagged encoding (ASHRAE 135 clause 20.2).
//!
//! A service's parameters are a stream of tags. Application tags carry their
//! data type (REAL, UNSIGNED, …), so a reader can decode them blind; that is
//! how property values travel. Context tags are the numbered fields of a
//! service's ASN.1 sequence, and only the service definition says what type
//! each holds. Opening and closing tags bracket constructed values: a
//! property value of any type, a list of property references.

/// Application tag numbers.
const NULL: u8 = 0;
const BOOLEAN: u8 = 1;
const UNSIGNED: u8 = 2;
const SIGNED: u8 = 3;
const REAL: u8 = 4;
const DOUBLE: u8 = 5;
const OCTET_STRING: u8 = 6;
const CHARACTER_STRING: u8 = 7;
const BIT_STRING: u8 = 8;
const ENUMERATED: u8 = 9;
const DATE: u8 = 10;
const TIME: u8 = 11;
const OBJECT_ID: u8 = 12;

/// A BACnet object identifier: 10-bit type, 22-bit instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObjectId {
    pub object_type: u16,
    pub instance: u32,
}

impl ObjectId {
    pub const DEVICE: u16 = 8;
    /// A device instance meaning "whichever device received this request".
    pub const WILDCARD: u32 = 4_194_303;

    pub fn new(object_type: u16, instance: u32) -> Self {
        ObjectId {
            object_type,
            instance,
        }
    }

    fn to_u32(self) -> u32 {
        (self.object_type as u32 & 0x3FF) << 22 | (self.instance & 0x3F_FFFF)
    }

    fn from_u32(v: u32) -> Self {
        ObjectId {
            object_type: (v >> 22) as u16,
            instance: v & 0x3F_FFFF,
        }
    }
}

/// An application-tagged primitive value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Boolean(bool),
    Unsigned(u64),
    Signed(i64),
    Real(f32),
    Double(f64),
    OctetString(Vec<u8>),
    CharString(String),
    BitString(Vec<bool>),
    Enumerated(u32),
    Date([u8; 4]),
    Time([u8; 4]),
    ObjectId(ObjectId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tag {
    /// For BOOLEAN, `len` is the value itself; the tag has no content.
    App {
        number: u8,
        len: u32,
    },
    Context {
        number: u8,
        len: u32,
    },
    Open(u8),
    Close(u8),
}

/// Why a request couldn't be decoded. Each maps to a BACnet Reject reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
    InvalidTag,
    BadValue,
    TooManyArguments,
}

impl DecodeError {
    pub fn reject_reason(self) -> u8 {
        match self {
            DecodeError::Truncated => 5,        // missing-required-parameter
            DecodeError::InvalidTag => 4,       // invalid-tag
            DecodeError::BadValue => 3,         // invalid-parameter-data-type
            DecodeError::TooManyArguments => 7, // too-many-arguments
        }
    }
}

pub type DResult<T> = Result<T, DecodeError>;

// ---------------------------------------------------------------------------
// Encoding

/// A tag header for `len` bytes of content.
pub fn tag(buf: &mut Vec<u8>, number: u8, context: bool, len: usize) {
    let class = if context { 0x08 } else { 0 };
    let lvt = if len < 5 { len as u8 } else { 5 };
    if number <= 14 {
        buf.push(number << 4 | class | lvt);
    } else {
        buf.push(0xF0 | class | lvt);
        buf.push(number);
    }
    if len >= 5 {
        if len <= 253 {
            buf.push(len as u8);
        } else if len <= 0xFFFF {
            buf.push(254);
            buf.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            buf.push(255);
            buf.extend_from_slice(&(len as u32).to_be_bytes());
        }
    }
}

pub fn open(buf: &mut Vec<u8>, number: u8) {
    bracket(buf, number, 6);
}

pub fn close(buf: &mut Vec<u8>, number: u8) {
    bracket(buf, number, 7);
}

fn bracket(buf: &mut Vec<u8>, number: u8, lvt: u8) {
    if number <= 14 {
        buf.push(number << 4 | 0x08 | lvt);
    } else {
        buf.push(0xF8 | lvt);
        buf.push(number);
    }
}

fn primitive(buf: &mut Vec<u8>, number: u8, context: bool, content: &[u8]) {
    tag(buf, number, context, content.len());
    buf.extend_from_slice(content);
}

/// Big-endian bytes of `v` with leading zero bytes dropped (at least one).
fn unsigned_bytes(v: u64) -> ([u8; 8], usize) {
    (v.to_be_bytes(), ((v.leading_zeros() / 8) as usize).min(7))
}

/// Shortest two's-complement big-endian form of `v`.
fn signed_bytes(v: i64) -> ([u8; 8], usize) {
    let b = v.to_be_bytes();
    let mut start = 0;
    while start < 7 {
        let (hi, next) = (b[start], b[start + 1]);
        if (hi == 0x00 && next & 0x80 == 0) || (hi == 0xFF && next & 0x80 != 0) {
            start += 1;
        } else {
            break;
        }
    }
    (b, start)
}

pub fn app_null(buf: &mut Vec<u8>) {
    buf.push(NULL << 4);
}

pub fn app_bool(buf: &mut Vec<u8>, v: bool) {
    buf.push(BOOLEAN << 4 | v as u8);
}

pub fn app_unsigned(buf: &mut Vec<u8>, v: u64) {
    let (b, s) = unsigned_bytes(v);
    primitive(buf, UNSIGNED, false, &b[s..]);
}

pub fn app_signed(buf: &mut Vec<u8>, v: i64) {
    let (b, s) = signed_bytes(v);
    primitive(buf, SIGNED, false, &b[s..]);
}

pub fn app_real(buf: &mut Vec<u8>, v: f32) {
    primitive(buf, REAL, false, &v.to_be_bytes());
}

pub fn app_enumerated(buf: &mut Vec<u8>, v: u32) {
    let (b, s) = unsigned_bytes(v as u64);
    primitive(buf, ENUMERATED, false, &b[s..]);
}

/// A CharacterString in UTF-8 (character set 0).
pub fn app_char_string(buf: &mut Vec<u8>, s: &str) {
    tag(buf, CHARACTER_STRING, false, s.len() + 1);
    buf.push(0);
    buf.extend_from_slice(s.as_bytes());
}

pub fn app_bit_string(buf: &mut Vec<u8>, bits: &[bool]) {
    let octets = bits.len().div_ceil(8);
    tag(buf, BIT_STRING, false, octets + 1);
    buf.push(((8 - bits.len() % 8) % 8) as u8);
    for chunk in bits.chunks(8) {
        buf.push(
            chunk
                .iter()
                .enumerate()
                .fold(0u8, |acc, (i, &b)| acc | (b as u8) << (7 - i)),
        );
    }
}

pub fn app_date(buf: &mut Vec<u8>, d: [u8; 4]) {
    primitive(buf, DATE, false, &d);
}

pub fn app_time(buf: &mut Vec<u8>, t: [u8; 4]) {
    primitive(buf, TIME, false, &t);
}

pub fn app_object_id(buf: &mut Vec<u8>, id: ObjectId) {
    primitive(buf, OBJECT_ID, false, &id.to_u32().to_be_bytes());
}

pub fn ctx_unsigned(buf: &mut Vec<u8>, number: u8, v: u64) {
    let (b, s) = unsigned_bytes(v);
    primitive(buf, number, true, &b[s..]);
}

pub fn ctx_enumerated(buf: &mut Vec<u8>, number: u8, v: u32) {
    ctx_unsigned(buf, number, v as u64);
}

pub fn ctx_object_id(buf: &mut Vec<u8>, number: u8, id: ObjectId) {
    primitive(buf, number, true, &id.to_u32().to_be_bytes());
}

// ---------------------------------------------------------------------------
// Decoding

#[derive(Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    pub fn at_end(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn byte(&mut self) -> DResult<u8> {
        let b = *self.buf.get(self.pos).ok_or(DecodeError::Truncated)?;
        self.pos += 1;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> DResult<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.buf.len())
            .ok_or(DecodeError::Truncated)?;
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    pub fn read_tag(&mut self) -> DResult<Tag> {
        let first = self.byte()?;
        let number = if first >> 4 == 0x0F {
            self.byte()?
        } else {
            first >> 4
        };
        let context = first & 0x08 != 0;
        let lvt = first & 0x07;
        match (context, lvt) {
            (true, 6) => return Ok(Tag::Open(number)),
            (true, 7) => return Ok(Tag::Close(number)),
            (false, 6 | 7) => return Err(DecodeError::InvalidTag),
            _ => {}
        }
        let len = if lvt == 5 {
            match self.byte()? {
                254 => u16::from_be_bytes(self.take(2)?.try_into().unwrap()) as u32,
                255 => u32::from_be_bytes(self.take(4)?.try_into().unwrap()),
                n => n as u32,
            }
        } else {
            lvt as u32
        };
        Ok(if context {
            Tag::Context { number, len }
        } else {
            Tag::App { number, len }
        })
    }

    pub fn peek_tag(&self) -> Option<Tag> {
        self.clone().read_tag().ok()
    }

    pub fn peek_context(&self, number: u8) -> bool {
        matches!(self.peek_tag(), Some(Tag::Context { number: n, .. }) if n == number)
    }

    pub fn peek_close(&self, number: u8) -> bool {
        self.peek_tag() == Some(Tag::Close(number))
    }

    pub fn expect_open(&mut self, number: u8) -> DResult<()> {
        match self.read_tag()? {
            Tag::Open(n) if n == number => Ok(()),
            _ => Err(DecodeError::InvalidTag),
        }
    }

    pub fn expect_close(&mut self, number: u8) -> DResult<()> {
        match self.read_tag()? {
            Tag::Close(n) if n == number => Ok(()),
            _ => Err(DecodeError::InvalidTag),
        }
    }

    /// The content of a required context-tagged primitive.
    pub fn ctx_raw(&mut self, number: u8) -> DResult<&'a [u8]> {
        if self.at_end() {
            return Err(DecodeError::Truncated);
        }
        match self.read_tag()? {
            Tag::Context { number: n, len } if n == number => self.take(len as usize),
            _ => Err(DecodeError::InvalidTag),
        }
    }

    /// The content of an optional context-tagged primitive, if it's next.
    pub fn opt_ctx_raw(&mut self, number: u8) -> DResult<Option<&'a [u8]>> {
        if self.peek_context(number) {
            self.ctx_raw(number).map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn ctx_unsigned(&mut self, number: u8) -> DResult<u64> {
        decode_unsigned(self.ctx_raw(number)?)
    }

    pub fn opt_ctx_unsigned(&mut self, number: u8) -> DResult<Option<u64>> {
        self.opt_ctx_raw(number)?.map(decode_unsigned).transpose()
    }

    pub fn ctx_enumerated(&mut self, number: u8) -> DResult<u32> {
        let v = self.ctx_unsigned(number)?;
        u32::try_from(v).map_err(|_| DecodeError::BadValue)
    }

    pub fn ctx_object_id(&mut self, number: u8) -> DResult<ObjectId> {
        decode_object_id(self.ctx_raw(number)?)
    }

    pub fn opt_ctx_bool(&mut self, number: u8) -> DResult<Option<bool>> {
        Ok(self
            .opt_ctx_raw(number)?
            .map(|c| c.first().is_some_and(|&b| b != 0)))
    }

    pub fn ctx_char_string(&mut self, number: u8) -> DResult<String> {
        decode_char_string(self.ctx_raw(number)?)
    }

    /// One application-tagged value.
    pub fn read_app(&mut self) -> DResult<Value> {
        let Tag::App { number, len } = self.read_tag()? else {
            return Err(DecodeError::InvalidTag);
        };
        if number == BOOLEAN {
            return match len {
                0 | 1 => Ok(Value::Boolean(len == 1)),
                _ => Err(DecodeError::BadValue),
            };
        }
        let c = self.take(len as usize)?;
        let fixed = |n: usize| {
            if c.len() == n {
                Ok(())
            } else {
                Err(DecodeError::BadValue)
            }
        };
        Ok(match number {
            NULL => {
                fixed(0)?;
                Value::Null
            }
            UNSIGNED => Value::Unsigned(decode_unsigned(c)?),
            SIGNED => Value::Signed(decode_signed(c)?),
            REAL => {
                fixed(4)?;
                Value::Real(f32::from_be_bytes(c.try_into().unwrap()))
            }
            DOUBLE => {
                fixed(8)?;
                Value::Double(f64::from_be_bytes(c.try_into().unwrap()))
            }
            OCTET_STRING => Value::OctetString(c.to_vec()),
            CHARACTER_STRING => Value::CharString(decode_char_string(c)?),
            BIT_STRING => Value::BitString(decode_bit_string(c)?),
            ENUMERATED => Value::Enumerated(
                u32::try_from(decode_unsigned(c)?).map_err(|_| DecodeError::BadValue)?,
            ),
            DATE => {
                fixed(4)?;
                Value::Date(c.try_into().unwrap())
            }
            TIME => {
                fixed(4)?;
                Value::Time(c.try_into().unwrap())
            }
            OBJECT_ID => Value::ObjectId(decode_object_id(c)?),
            _ => return Err(DecodeError::InvalidTag),
        })
    }

    /// A property value bracketed by opening/closing tag `number`: the
    /// application values inside, or None when it holds constructed data
    /// (context tags, nested brackets) that no writable property here takes.
    pub fn property_value(&mut self, number: u8) -> DResult<Option<Vec<Value>>> {
        self.expect_open(number)?;
        let mut values = Vec::new();
        let mut constructed = false;
        let mut depth = 0u32;
        loop {
            match self.peek_tag().ok_or(DecodeError::Truncated)? {
                Tag::Close(n) if depth == 0 && n == number => {
                    self.read_tag()?;
                    break;
                }
                Tag::App { .. } if depth == 0 => values.push(self.read_app()?),
                _ => {
                    constructed = true;
                    match self.read_tag()? {
                        Tag::Open(_) => depth += 1,
                        Tag::Close(_) => {
                            depth = depth.checked_sub(1).ok_or(DecodeError::InvalidTag)?
                        }
                        Tag::App {
                            number: BOOLEAN, ..
                        } => {}
                        Tag::App { len, .. } | Tag::Context { len, .. } => {
                            self.take(len as usize)?;
                        }
                    }
                }
            }
        }
        Ok(if constructed { None } else { Some(values) })
    }
}

pub fn decode_unsigned(c: &[u8]) -> DResult<u64> {
    if c.is_empty() || c.len() > 8 {
        return Err(DecodeError::BadValue);
    }
    Ok(c.iter().fold(0u64, |acc, &b| acc << 8 | b as u64))
}

fn decode_signed(c: &[u8]) -> DResult<i64> {
    if c.is_empty() || c.len() > 8 {
        return Err(DecodeError::BadValue);
    }
    let init = if c[0] & 0x80 != 0 { -1i64 } else { 0 };
    Ok(c.iter().fold(init, |acc, &b| acc << 8 | b as i64))
}

fn decode_object_id(c: &[u8]) -> DResult<ObjectId> {
    let b: [u8; 4] = c.try_into().map_err(|_| DecodeError::BadValue)?;
    Ok(ObjectId::from_u32(u32::from_be_bytes(b)))
}

fn decode_char_string(c: &[u8]) -> DResult<String> {
    let (&charset, text) = c.split_first().ok_or(DecodeError::BadValue)?;
    Ok(match charset {
        // UCS-2, big-endian
        4 => char::decode_utf16(
            text.as_chunks::<2>()
                .0
                .iter()
                .map(|&p| u16::from_be_bytes(p)),
        )
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect(),
        // ISO 8859-1
        5 => text.iter().map(|&b| b as char).collect(),
        // UTF-8, and anything else read as if it were
        _ => String::from_utf8_lossy(text).into_owned(),
    })
}

fn decode_bit_string(c: &[u8]) -> DResult<Vec<bool>> {
    let (&unused, octets) = c.split_first().ok_or(DecodeError::BadValue)?;
    if unused > 7 || (octets.is_empty() && unused != 0) {
        return Err(DecodeError::BadValue);
    }
    let n = octets.len() * 8 - unused as usize;
    Ok((0..n)
        .map(|i| octets[i / 8] & (0x80 >> (i % 8)) != 0)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Value {
        fn encode(&self, buf: &mut Vec<u8>) {
            match self {
                Value::Null => app_null(buf),
                Value::Boolean(b) => app_bool(buf, *b),
                Value::Unsigned(v) => app_unsigned(buf, *v),
                Value::Signed(v) => app_signed(buf, *v),
                Value::Real(v) => app_real(buf, *v),
                Value::Double(v) => primitive(buf, DOUBLE, false, &v.to_be_bytes()),
                Value::OctetString(v) => primitive(buf, OCTET_STRING, false, v),
                Value::CharString(s) => app_char_string(buf, s),
                Value::BitString(bits) => app_bit_string(buf, bits),
                Value::Enumerated(v) => app_enumerated(buf, *v),
                Value::Date(d) => app_date(buf, *d),
                Value::Time(t) => app_time(buf, *t),
                Value::ObjectId(id) => app_object_id(buf, *id),
            }
        }
    }

    fn round_trip(v: Value) -> Vec<u8> {
        let mut buf = Vec::new();
        v.encode(&mut buf);
        let mut r = Reader::new(&buf);
        assert_eq!(r.read_app().unwrap(), v, "bytes {buf:02X?}");
        assert!(r.at_end());
        buf
    }

    #[test]
    fn primitives_round_trip() {
        assert_eq!(round_trip(Value::Null), [0x00]);
        assert_eq!(round_trip(Value::Boolean(true)), [0x11]);
        assert_eq!(round_trip(Value::Boolean(false)), [0x10]);
        assert_eq!(round_trip(Value::Unsigned(0)), [0x21, 0x00]);
        assert_eq!(round_trip(Value::Unsigned(255)), [0x21, 0xFF]);
        assert_eq!(round_trip(Value::Unsigned(256)), [0x22, 0x01, 0x00]);
        assert_eq!(round_trip(Value::Unsigned(1476)), [0x22, 0x05, 0xC4]);
        assert_eq!(
            round_trip(Value::Unsigned(u32::MAX as u64)),
            [0x24, 0xFF, 0xFF, 0xFF, 0xFF]
        );
        // Five content bytes need the extended length octet.
        assert_eq!(
            round_trip(Value::Unsigned(1 << 32)),
            [0x25, 0x05, 0x01, 0, 0, 0, 0]
        );
        assert_eq!(round_trip(Value::Signed(0)), [0x31, 0x00]);
        assert_eq!(round_trip(Value::Signed(-1)), [0x31, 0xFF]);
        assert_eq!(round_trip(Value::Signed(127)), [0x31, 0x7F]);
        assert_eq!(round_trip(Value::Signed(128)), [0x32, 0x00, 0x80]);
        assert_eq!(round_trip(Value::Signed(-128)), [0x31, 0x80]);
        assert_eq!(round_trip(Value::Signed(-129)), [0x32, 0xFF, 0x7F]);
        assert_eq!(round_trip(Value::Signed(-300)), [0x32, 0xFE, 0xD4]);
        assert_eq!(
            round_trip(Value::Real(72.5)),
            [0x44, 0x42, 0x91, 0x00, 0x00]
        );
        round_trip(Value::Double(-1.25e100));
        assert_eq!(round_trip(Value::Enumerated(3)), [0x91, 0x03]);
        assert_eq!(
            round_trip(Value::Date([126, 10, 30, 5])),
            [0xA4, 126, 10, 30, 5]
        );
        round_trip(Value::Time([13, 45, 7, 50]));
        // analog-value 1 → (2 << 22) | 1
        assert_eq!(
            round_trip(Value::ObjectId(ObjectId::new(2, 1))),
            [0xC4, 0x00, 0x80, 0x00, 0x01]
        );
        round_trip(Value::ObjectId(ObjectId::new(
            ObjectId::DEVICE,
            ObjectId::WILDCARD,
        )));
        round_trip(Value::OctetString(vec![1, 2, 3]));
    }

    #[test]
    fn char_strings_are_utf8_with_extended_lengths() {
        assert_eq!(
            round_trip(Value::CharString("AV".into())),
            [0x73, 0x00, b'A', b'V']
        );
        assert_eq!(round_trip(Value::CharString(String::new())), [0x71, 0x00]);
        // 4 chars + charset = 5: extended length, one octet
        assert_eq!(
            &round_trip(Value::CharString("ZN-T".into()))[..3],
            [0x75, 0x05, 0x00]
        );
        assert_eq!(round_trip(Value::CharString("72 °F".into()))[1], 7);
        // 253 is the longest one-octet extended length; 254 switches to two.
        assert_eq!(
            &round_trip(Value::CharString("x".repeat(252)))[..2],
            [0x75, 253]
        );
        assert_eq!(
            &round_trip(Value::CharString("x".repeat(253)))[..4],
            [0x75, 254, 0x00, 254]
        );
        assert_eq!(
            &round_trip(Value::CharString("x".repeat(70_000)))[..6],
            [0x75, 255, 0x00, 0x01, 0x11, 0x71]
        );
    }

    #[test]
    fn foreign_character_sets_decode() {
        // UCS-2 "Hi" and ISO 8859-1 "°"
        let mut r = Reader::new(&[0x75, 0x05, 0x04, 0x00, b'H', 0x00, b'i']);
        assert_eq!(r.read_app().unwrap(), Value::CharString("Hi".into()));
        let mut r = Reader::new(&[0x72, 0x05, 0xB0]);
        assert_eq!(r.read_app().unwrap(), Value::CharString("°".into()));
    }

    #[test]
    fn bit_strings() {
        // status-flags: in-alarm, fault, overridden, out-of-service
        assert_eq!(
            round_trip(Value::BitString(vec![false, true, false, true])),
            [0x82, 0x04, 0x50]
        );
        assert_eq!(
            round_trip(Value::BitString(vec![true; 8])),
            [0x82, 0x00, 0xFF]
        );
        assert_eq!(
            round_trip(Value::BitString(vec![true; 9])),
            [0x83, 0x07, 0xFF, 0x80]
        );
        assert_eq!(round_trip(Value::BitString(vec![])), [0x81, 0x00]);
    }

    #[test]
    fn context_tags_and_extended_tag_numbers() {
        let mut buf = Vec::new();
        ctx_object_id(&mut buf, 0, ObjectId::new(8, 1234));
        ctx_enumerated(&mut buf, 1, 85);
        ctx_unsigned(&mut buf, 2, 0);
        ctx_unsigned(&mut buf, 20, 1_000_000); // tag 20 needs the extended tag octet
        open(&mut buf, 3);
        open(&mut buf, 33);
        close(&mut buf, 33);
        close(&mut buf, 3);
        assert_eq!(&buf[..5], [0x0C, 0x02, 0x00, 0x04, 0xD2]);
        assert_eq!(&buf[5..9], [0x19, 85, 0x29, 0x00]);
        assert_eq!(&buf[9..14], [0xFB, 20, 0x0F, 0x42, 0x40]);
        assert_eq!(&buf[14..], [0x3E, 0xFE, 33, 0xFF, 33, 0x3F]);

        let mut r = Reader::new(&buf);
        assert_eq!(r.ctx_object_id(0).unwrap(), ObjectId::new(8, 1234));
        assert_eq!(r.ctx_enumerated(1).unwrap(), 85);
        assert_eq!(r.opt_ctx_unsigned(3).unwrap(), None);
        assert_eq!(r.opt_ctx_unsigned(2).unwrap(), Some(0));
        assert_eq!(r.ctx_unsigned(20).unwrap(), 1_000_000);
        assert_eq!(r.peek_tag(), Some(Tag::Open(3)));
        r.expect_open(3).unwrap();
        assert_eq!(r.read_tag().unwrap(), Tag::Open(33));
        assert_eq!(r.read_tag().unwrap(), Tag::Close(33));
        assert!(r.peek_close(3));
        r.expect_close(3).unwrap();
        assert!(r.at_end());
        assert_eq!(r.ctx_unsigned(0), Err(DecodeError::Truncated));
    }

    #[test]
    fn priority_array_with_nulls() {
        let mut slots: Vec<Value> = vec![Value::Null; 16];
        slots[7] = Value::Real(74.0);
        slots[15] = Value::Real(70.0);
        let mut buf = Vec::new();
        open(&mut buf, 3);
        slots.iter().for_each(|v| v.encode(&mut buf));
        close(&mut buf, 3);
        let mut r = Reader::new(&buf);
        assert_eq!(r.property_value(3).unwrap(), Some(slots));
        assert!(r.at_end());
    }

    #[test]
    fn property_value_flags_constructed_data() {
        let mut buf = Vec::new();
        open(&mut buf, 3);
        ctx_unsigned(&mut buf, 0, 5);
        open(&mut buf, 1);
        app_real(&mut buf, 1.0);
        close(&mut buf, 1);
        close(&mut buf, 3);
        app_unsigned(&mut buf, 8);
        let mut r = Reader::new(&buf);
        assert_eq!(r.property_value(3).unwrap(), None);
        assert_eq!(r.read_app().unwrap(), Value::Unsigned(8));
    }

    #[test]
    fn malformed_input_is_rejected() {
        assert_eq!(
            Reader::new(&[0x44, 0x42]).read_app(),
            Err(DecodeError::Truncated)
        );
        assert_eq!(
            Reader::new(&[0x43, 1, 2, 3]).read_app(),
            Err(DecodeError::BadValue)
        );
        assert_eq!(
            Reader::new(&[0x0E]).read_app(),
            Err(DecodeError::InvalidTag)
        );
        assert_eq!(Reader::new(&[0x12]).read_app(), Err(DecodeError::BadValue));
        assert_eq!(
            Reader::new(&[0x26]).read_tag(),
            Err(DecodeError::InvalidTag)
        );
        assert_eq!(
            Reader::new(&[0x75, 255, 0xFF, 0xFF, 0xFF, 0xFF]).read_app(),
            Err(DecodeError::Truncated)
        );
        assert_eq!(
            Reader::new(&[0x19, 0x01]).ctx_object_id(0),
            Err(DecodeError::InvalidTag)
        );
        assert_eq!(
            Reader::new(&[0x0A, 0x01, 0x02]).ctx_object_id(0),
            Err(DecodeError::BadValue)
        );
        let mut buf = Vec::new();
        open(&mut buf, 3);
        app_real(&mut buf, 1.0);
        assert_eq!(
            Reader::new(&buf).property_value(3),
            Err(DecodeError::Truncated)
        );
    }
}
