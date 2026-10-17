//! Pure, bounded codecs. See docs/protocol for provenance and units.
pub mod ftms_control;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("truncated packet at byte {0}")]
    Truncated(usize),
    #[error("invalid packet: {0}")]
    Invalid(&'static str),
}
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(DecodeError::Invalid("length overflow"))?;
        let part = self
            .bytes
            .get(self.pos..end)
            .ok_or(DecodeError::Truncated(self.pos))?;
        self.pos = end;
        Ok(part)
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn i16(&mut self) -> Result<i16, DecodeError> {
        let b = self.take(2)?;
        Ok(i16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn finish(self) -> Result<(), DecodeError> {
        if self.pos == self.bytes.len() {
            Ok(())
        } else {
            Err(DecodeError::Invalid("trailing bytes"))
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn unhex(input: &str) -> Result<Vec<u8>, DecodeError> {
    let s: String = input
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != ':')
        .collect();
    if !s.is_ascii() || !s.len().is_multiple_of(2) || s.len() > 8192 {
        return Err(DecodeError::Invalid(
            "hex must contain at most 4096 byte pairs",
        ));
    }
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| {
            let digit = |c: u8| {
                (c as char)
                    .to_digit(16)
                    .map(|v| v as u8)
                    .ok_or(DecodeError::Invalid("non-hex digit"))
            };
            Ok(digit(p[0])? * 16 + digit(p[1])?)
        })
        .collect()
}

pub fn indoor_bike(bytes: &[u8]) -> Result<Value, DecodeError> {
    let mut c = Cursor::new(bytes);
    let flags = c.u16()?;
    if flags & 0xe000 != 0 {
        return Err(DecodeError::Invalid("reserved indoor bike flags"));
    }
    let mut o = Map::new();
    o.insert("flags".into(), json!(flags));
    if flags & 1 == 0 {
        o.insert("speed_kmh".into(), json!(f64::from(c.u16()?) / 100.0));
    }
    for bit in 1..=12 {
        if flags & (1 << bit) == 0 {
            continue;
        }
        match bit {
            1 => {
                o.insert(
                    "average_speed_kmh".into(),
                    json!(f64::from(c.u16()?) / 100.0),
                );
            }
            2 => {
                o.insert("cadence_rpm".into(), json!(f64::from(c.u16()?) / 2.0));
            }
            3 => {
                o.insert(
                    "average_cadence_rpm".into(),
                    json!(f64::from(c.u16()?) / 2.0),
                );
            }
            4 => {
                let b = c.take(3)?;
                o.insert(
                    "distance_m".into(),
                    json!(u32::from_le_bytes([b[0], b[1], b[2], 0])),
                );
            }
            5 => {
                o.insert("resistance_level".into(), json!(f64::from(c.i16()?) / 10.0));
            }
            6 => {
                o.insert("power_w".into(), json!(c.i16()?));
            }
            7 => {
                o.insert("average_power_w".into(), json!(c.i16()?));
            }
            8 => {
                o.insert("energy_kcal".into(), json!(c.u16()?));
                o.insert("energy_kcal_hour".into(), json!(c.u16()?));
                o.insert("energy_kcal_minute".into(), json!(c.u8()?));
            }
            9 => {
                o.insert("heart_rate_bpm".into(), json!(c.u8()?));
            }
            10 => {
                o.insert(
                    "metabolic_equivalent".into(),
                    json!(f64::from(c.u8()?) / 10.0),
                );
            }
            11 => {
                o.insert("elapsed_seconds".into(), json!(c.u16()?));
            }
            12 => {
                o.insert("remaining_seconds".into(), json!(c.u16()?));
            }
            _ => {}
        }
    }
    c.finish()?;
    Ok(Value::Object(o))
}

pub fn cycling_power(bytes: &[u8]) -> Result<Value, DecodeError> {
    let mut c = Cursor::new(bytes);
    let flags = c.u16()?;
    if flags & 0xe000 != 0 {
        return Err(DecodeError::Invalid("reserved cycling power flags"));
    }
    let mut o = Map::new();
    o.insert("flags".into(), json!(flags));
    o.insert("power_w".into(), json!(c.i16()?));
    if flags & 1 != 0 {
        o.insert(
            "pedal_balance_percent".into(),
            json!(f64::from(c.u8()?) / 2.0),
        );
    }
    if flags & 4 != 0 {
        o.insert(
            "accumulated_torque_nm".into(),
            json!(f64::from(c.u16()?) / 32.0),
        );
    }
    if flags & 16 != 0 {
        o.insert("wheel_revolutions".into(), json!(c.u32()?));
        o.insert("wheel_event_2048hz".into(), json!(c.u16()?));
    }
    if flags & 32 != 0 {
        o.insert("crank_revolutions".into(), json!(c.u16()?));
        o.insert("crank_event_1024hz".into(), json!(c.u16()?));
    }
    if flags & 64 != 0 {
        o.insert("maximum_force_n".into(), json!(c.i16()?));
        o.insert("minimum_force_n".into(), json!(c.i16()?));
    }
    if flags & 128 != 0 {
        o.insert(
            "maximum_torque_nm".into(),
            json!(f64::from(c.i16()?) / 32.0),
        );
        o.insert(
            "minimum_torque_nm".into(),
            json!(f64::from(c.i16()?) / 32.0),
        );
    }
    if flags & 256 != 0 {
        let b = c.take(3)?;
        let v = u32::from_le_bytes([b[0], b[1], b[2], 0]);
        o.insert("maximum_angle_degrees".into(), json!(v & 0xfff));
        o.insert("minimum_angle_degrees".into(), json!(v >> 12));
    }
    if flags & 512 != 0 {
        o.insert("top_dead_spot_degrees".into(), json!(c.u16()?));
    }
    if flags & 1024 != 0 {
        o.insert("bottom_dead_spot_degrees".into(), json!(c.u16()?));
    }
    if flags & 2048 != 0 {
        o.insert("accumulated_energy_kj".into(), json!(c.u16()?));
    }
    c.finish()?;
    Ok(Value::Object(o))
}
pub fn csc(bytes: &[u8]) -> Result<Value, DecodeError> {
    let mut c = Cursor::new(bytes);
    let f = c.u8()?;
    if f & !3 != 0 {
        return Err(DecodeError::Invalid("reserved CSC flags"));
    }
    let mut o = Map::new();
    o.insert("flags".into(), json!(f));
    if f & 1 != 0 {
        o.insert("wheel_revolutions".into(), json!(c.u32()?));
        o.insert("wheel_event_1024hz".into(), json!(c.u16()?));
    }
    if f & 2 != 0 {
        o.insert("crank_revolutions".into(), json!(c.u16()?));
        o.insert("crank_event_1024hz".into(), json!(c.u16()?));
    }
    c.finish()?;
    Ok(Value::Object(o))
}
/// Decode a standard 16-bit Bluetooth characteristic, preserving unknown data as hex.
pub fn characteristic(id: u16, bytes: &[u8]) -> Result<Value, DecodeError> {
    match id {
        0x2ad2 => indoor_bike(bytes),
        0x2a63 => cycling_power(bytes),
        0x2a5b => csc(bytes),
        0x2a19 => {
            let mut c = Cursor::new(bytes);
            let n = c.u8()?;
            c.finish()?;
            if n > 100 {
                return Err(DecodeError::Invalid("battery above 100"));
            }
            Ok(json!({"battery_percent":n}))
        }
        0x2acc => {
            let mut c = Cursor::new(bytes);
            let features = c.u32()?;
            let targets = c.u32()?;
            c.finish()?;
            Ok(json!({"fitness_machine_features":features,"target_setting_features":targets}))
        }
        0x2a65 => {
            let mut c = Cursor::new(bytes);
            let f = c.u32()?;
            c.finish()?;
            Ok(json!({"cycling_power_features":f}))
        }
        0x2a5c => {
            let mut c = Cursor::new(bytes);
            let f = c.u16()?;
            c.finish()?;
            Ok(json!({"csc_features":f}))
        }
        0x2ad6 | 0x2ad8 => {
            let mut c = Cursor::new(bytes);
            let min = c.i16()?;
            let max = c.i16()?;
            let inc = c.u16()?;
            c.finish()?;
            if min > max {
                return Err(DecodeError::Invalid("inverted range"));
            }
            let scale = if id == 0x2ad6 { 10.0 } else { 1.0 };
            Ok(
                json!({"minimum":f64::from(min)/scale,"maximum":f64::from(max)/scale,"increment":f64::from(inc)/scale,"unit":if id==0x2ad6 {"resistance_level"}else{"W"}}),
            )
        }
        0x2a24..=0x2a29 => {
            let s = std::str::from_utf8(bytes)
                .map_err(|_| DecodeError::Invalid("invalid UTF8 identity"))?;
            Ok(json!({"text":s.trim_end_matches('\0')}))
        }
        _ => Ok(json!({"raw_hex":hex(bytes)})),
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    ShiftUp,
    ShiftDown,
}
impl Action {
    pub fn id(self) -> u8 {
        match self {
            Self::ShiftUp => 1,
            Self::ShiftDown => 2,
        }
    }
    pub fn transition(self, pressed: bool) -> [u8; 3] {
        [1, self.id(), u8::from(pressed)]
    }
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppInfo {
    pub id: String,
    pub version: String,
    pub buttons: Vec<u8>,
}
impl AppInfo {
    pub fn supports(&self, action: Action) -> bool {
        self.buttons.is_empty() || self.buttons.contains(&action.id())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppMessage {
    Info(AppInfo),
    Haptic {
        pattern: u8,
        duration: u8,
        intensity: u8,
    },
    /// Type not in the specification (seen from MyWhoosh 5.7.0). Without a length field the
    /// rest of the buffered bytes cannot be split, so they are returned together and dropped.
    Unknown(Vec<u8>),
}
/// A bounded stream decoder for app-to-device frames (never TCP-read boundaries).
#[derive(Debug, Default)]
pub struct AppDecoder {
    buffer: Vec<u8>,
}
impl AppDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<AppMessage>, DecodeError> {
        if self.buffer.len() + bytes.len() > 4096 {
            return Err(DecodeError::Invalid("app stream exceeds 4096 bytes"));
        }
        self.buffer.extend_from_slice(bytes);
        let mut result = Vec::new();
        while let Some(&kind) = self.buffer.first() {
            if !matches!(kind, 3 | 4) {
                result.push(AppMessage::Unknown(std::mem::take(&mut self.buffer)));
                break;
            }
            let mut c = Cursor::new(&self.buffer);
            let parsed = (|| -> Result<AppMessage, DecodeError> {
                c.u8()?;
                match kind {
                    3 => Ok(AppMessage::Haptic {
                        pattern: c.u8()?,
                        duration: c.u8()?,
                        intensity: c.u8()?,
                    }),
                    4 => {
                        if c.u8()? != 1 {
                            return Err(DecodeError::Invalid("unsupported app-info version"));
                        }
                        let n = usize::from(c.u8()?);
                        if n > 32 {
                            return Err(DecodeError::Invalid("app ID too long"));
                        }
                        let id = std::str::from_utf8(c.take(n)?)
                            .map_err(|_| DecodeError::Invalid("app ID UTF8"))?
                            .to_owned();
                        let n = usize::from(c.u8()?);
                        if n > 32 {
                            return Err(DecodeError::Invalid("app version too long"));
                        }
                        let version = std::str::from_utf8(c.take(n)?)
                            .map_err(|_| DecodeError::Invalid("app version UTF8"))?
                            .to_owned();
                        let n = usize::from(c.u8()?);
                        let buttons = c.take(n)?.to_vec();
                        Ok(AppMessage::Info(AppInfo {
                            id,
                            version,
                            buttons,
                        }))
                    }
                    _ => Err(DecodeError::Invalid("unknown app-to-device message")),
                }
            })();
            match parsed {
                Ok(message) => {
                    let n = c.pos;
                    self.buffer.drain(..n);
                    result.push(message);
                }
                Err(DecodeError::Truncated(_)) => break,
                Err(e) => return Err(e),
            }
        }
        Ok(result)
    }
    pub fn is_pending(&self) -> bool {
        !self.buffer.is_empty()
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "tests assert valid fixtures")]
mod tests {
    use super::*;
    #[test]
    fn indoor_flags_units_and_signed_power() {
        let v = indoor_bike(&[0x44, 0, 0xc4, 9, 180, 0, 0xec, 0xff]).unwrap();
        assert_eq!(v["speed_kmh"], 25.0);
        assert_eq!(v["cadence_rpm"], 90.0);
        assert_eq!(v["power_w"], -20);
        assert!(v.get("heart_rate_bpm").is_none());
    }
    #[test]
    fn all_prefixes_are_rejected() {
        let p = [0x44, 0, 0xc4, 9, 180, 0, 200, 0];
        for n in 0..p.len() {
            assert!(indoor_bike(&p[..n]).is_err());
        }
        assert!(indoor_bike(&p).is_ok());
    }
    #[test]
    fn captured_x2_pedalling_frames() {
        // Real X2 Max frames, fw V3.2.3; see tests/fixtures/README.md.
        let v = indoor_bike(&unhex("44009b077c005200").unwrap()).unwrap();
        assert_eq!(v["speed_kmh"], 19.47);
        assert_eq!(v["cadence_rpm"], 62.0);
        assert_eq!(v["power_w"], 82);
        let v = cycling_power(&unhex("30005300ec01000053763900a4e9").unwrap()).unwrap();
        assert_eq!(v["power_w"], 83);
        assert_eq!(v["wheel_revolutions"], 492);
        assert_eq!(v["wheel_event_2048hz"], 30291);
        assert_eq!(v["crank_revolutions"], 57);
        assert_eq!(v["crank_event_1024hz"], 59812);
    }
    #[test]
    fn more_data_omits_speed() {
        assert!(indoor_bike(&[1, 0]).unwrap().get("speed_kmh").is_none());
    }
    #[test]
    fn cps_optional_fields() {
        let v = cycling_power(&[0x20, 0, 250, 0, 0xff, 0xff, 0, 4]).unwrap();
        assert_eq!(v["power_w"], 250);
        assert_eq!(v["crank_event_1024hz"], 1024);
        assert_eq!(v["crank_revolutions"], 65535);
    }
    #[test]
    fn malformed_packets() {
        assert!(indoor_bike(&[0, 0x80]).is_err());
        assert!(csc(&[4]).is_err());
        assert!(characteristic(0x2a19, &[101]).is_err());
        assert!(characteristic(0x2a19, &[50, 0]).is_err());
        assert!(unhex("é").is_err());
        assert!(unhex("a").is_err());
    }
    #[test]
    fn fragmented_and_coalesced_app_info() {
        let p = [4, 1, 1, b'x', 1, b'1', 2, 1, 2, 3, 1, 0, 0];
        for split in 0..=p.len() {
            let mut d = AppDecoder::default();
            let mut out = d.push(&p[..split]).unwrap();
            out.extend(d.push(&p[split..]).unwrap());
            assert_eq!(out.len(), 2);
            assert!(!d.is_pending());
        }
    }
    #[test]
    fn zero_buttons_means_all() {
        let mut d = AppDecoder::default();
        let m = d.push(&[4, 1, 0, 0, 0]).unwrap();
        let AppMessage::Info(i) = &m[0] else {
            panic!("wrong message")
        };
        assert!(i.supports(Action::ShiftDown));
    }
    #[test]
    fn unknown_app_message_is_dropped_not_fatal() {
        let mut d = AppDecoder::default();
        let m = d.push(&[4, 1, 0, 0, 0, 0x7e, 1, 2]).unwrap();
        assert!(matches!(m[0], AppMessage::Info(_)));
        assert_eq!(m[1], AppMessage::Unknown(vec![0x7e, 1, 2]));
        assert!(!d.is_pending());
        assert!(matches!(
            d.push(&[4, 1, 0, 0, 0]).unwrap()[0],
            AppMessage::Info(_)
        ));
    }
    #[test]
    fn stream_limits_and_version() {
        assert!(AppDecoder::default().push(&[4, 2]).is_err());
        assert!(AppDecoder::default().push(&[0; 4097]).is_err());
    }
    #[test]
    fn synthetic_robustness() {
        let mut seed = 1u32;
        for len in 0..128 {
            let p: Vec<u8> = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    (seed >> 24) as u8
                })
                .collect();
            let _ = indoor_bike(&p);
            let _ = cycling_power(&p);
            let _ = csc(&p);
            let _ = AppDecoder::default().push(&p);
        }
    }
    #[test]
    fn golden_shift() {
        assert_eq!(Action::ShiftUp.transition(true), [1, 1, 1]);
        assert_eq!(Action::ShiftDown.transition(false), [1, 2, 0]);
    }
    #[test]
    fn every_indoor_optional_layout_is_bounded() {
        // SIG field widths, independent synthetic encoder, all 8192 flag combinations.
        let widths = [2, 2, 2, 3, 2, 2, 2, 5, 1, 1, 2, 2];
        for flags in 0u16..8192 {
            let mut bytes = flags.to_le_bytes().to_vec();
            if flags & 1 == 0 {
                bytes.extend([0, 0]);
            }
            for (i, width) in widths.iter().enumerate() {
                if flags & (1 << (i + 1)) != 0 {
                    bytes.extend(std::iter::repeat_n(0, *width));
                }
            }
            assert!(indoor_bike(&bytes).is_ok(), "flags {flags:04x}");
            for n in 0..bytes.len() {
                assert!(
                    indoor_bike(&bytes[..n]).is_err(),
                    "flags {flags:04x}, length {n}"
                );
            }
            bytes.push(0);
            assert!(indoor_bike(&bytes).is_err());
        }
    }
    #[test]
    fn coalesced_app_updates_and_invalid_utf8() {
        let mut d = AppDecoder::default();
        let frames = d.push(&[4, 1, 0, 0, 1, 1, 4, 1, 0, 0, 0]).unwrap();
        assert_eq!(frames.len(), 2);
        assert!(AppDecoder::default().push(&[4, 1, 1, 255, 0, 0]).is_err());
        assert!(AppDecoder::default().push(&[4, 1, 33]).is_err());
    }
}
