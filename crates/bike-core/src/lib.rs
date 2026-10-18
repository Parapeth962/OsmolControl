//! Transport-independent lifecycle and exact packet mapping.
use bike_protocol::Action;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// User-visible product name (also advertised to MyWhoosh). The command stays `bikectl`.
pub const APP_NAME: &str = "OsmolControl";

/// A known controller: how to recognise it in a scan and how to decode its button messages.
/// Built-in profiles come from [`builtin_controllers`]; users add more as JSON (same fields),
/// usually written by the app's "learn a controller" step ([`learn_profile`]).
///
/// Buttons are decoded one of two ways: `shift_up_hex`/`shift_down_hex` for controllers that send
/// one fixed message per click, or `buttons` for controllers that report which buttons are down.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerProfile {
    pub label: String,
    /// Advertised local-name prefixes, ignoring case; vendor names often end in a per-unit serial.
    #[serde(default)]
    pub name_prefixes: Vec<String>,
    /// Advertised service UUIDs, for controllers whose name varies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<String>,
    pub characteristic: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub shift_up_hex: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub shift_down_hex: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buttons: Option<ButtonMap>,
    /// Where the frames come from (a capture, a document); optional for user profiles.
    #[serde(default)]
    pub evidence: String,
}
/// Button-state decoding: a press is a button going from released to pressed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ButtonMap {
    pub shift_up: Vec<ButtonBits>,
    pub shift_down: Vec<ButtonBits>,
    /// The first message after connecting is the resting state, never a press.
    #[serde(default)]
    pub ignore_first_frame: bool,
}
/// One button, as bits of the message counted from its first byte.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ButtonBits {
    pub mask: String,
    /// Pressed when the masked bits equal this (same length as `mask`); without it, pressed
    /// when any masked bit is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}
impl ControllerProfile {
    pub fn matches(&self, name: &str, services: &[String]) -> bool {
        let name = name.to_lowercase();
        self.name_prefixes
            .iter()
            .any(|p| !p.is_empty() && name.starts_with(&p.to_lowercase()))
            || self
                .services
                .iter()
                .any(|s| services.iter().any(|a| a.eq_ignore_ascii_case(s)))
    }
    /// Whether the profile can be used: a label, something to match and a valid decoder.
    pub fn check(&self) -> Result<(), String> {
        if self.label.trim().is_empty() {
            return Err("every controller needs a label".into());
        }
        if self
            .name_prefixes
            .iter()
            .chain(&self.services)
            .all(|n| n.trim().is_empty())
        {
            return Err(format!("{}: name_prefixes must not be empty", self.label));
        }
        self.packet_map()
            .map(|_| ())
            .map_err(|e| format!("{}: {e}", self.label))
    }
    pub fn packet_map(&self) -> Result<PacketMap, String> {
        let evidence = if self.evidence.trim().is_empty() {
            format!("user profile {}", self.label)
        } else {
            self.evidence.clone()
        };
        match &self.buttons {
            None => PacketMap::new(ControllerMap {
                characteristic: self.characteristic.clone(),
                shift_up_hex: self.shift_up_hex.clone(),
                shift_down_hex: self.shift_down_hex.clone(),
                evidence,
            }),
            Some(_) if !self.shift_up_hex.is_empty() || !self.shift_down_hex.is_empty() => {
                Err("use either buttons or shift_up_hex/shift_down_hex, not both".into())
            }
            Some(b) => {
                let parse = |list: &[ButtonBits]| -> Result<Vec<Bits>, String> {
                    if list.is_empty() {
                        return Err("buttons need at least one shift_up and shift_down".into());
                    }
                    list.iter().map(Bits::new).collect()
                };
                Ok(PacketMap {
                    characteristic: self.characteristic.clone(),
                    decoder: Decoder::Buttons {
                        up: parse(&b.shift_up)?,
                        down: parse(&b.shift_down)?,
                        ignore_first: b.ignore_first_frame,
                        seen: false,
                    },
                })
            }
        }
    }
}
/// Pressed when byte `byte` equals `value`.
fn byte_is(byte: usize, value: u8) -> ButtonBits {
    ButtonBits {
        mask: format!("{}ff", "00".repeat(byte)),
        value: Some(format!("{}{value:02x}", "00".repeat(byte))),
    }
}
/// Pressed when any bit of `mask` is set in byte `byte`.
fn bit(byte: usize, mask: u8) -> ButtonBits {
    ButtonBits {
        mask: format!("{}{mask:02x}", "00".repeat(byte)),
        value: None,
    }
}
/// Built-in controllers. Add an entry only from a labelled capture or a documented protocol.
pub fn builtin_controllers() -> Vec<ControllerProfile> {
    vec![
        ControllerProfile {
            label: "ThinkRider virtual shifter (V100, VS100, VS200)".into(),
            // Every model advertises "THINK VS01-<serial>"; the V100 was captured, the VS200 frames
            // are the same in OpenBikeControl's BikeControl.
            name_prefixes: vec!["Think VS".into()],
            services: Vec::new(),
            characteristic: "0000fea1-0000-1000-8000-00805f9b34fb".into(),
            // The button printed '+' sends f3050301fc (shift up, harder gear).
            shift_up_hex: "f3050301fc".into(),
            shift_down_hex: "f3050300fb".into(),
            buttons: None,
            evidence: "docs/devices/v100.md (captures, fw V1.0.5)".into(),
        },
        ControllerProfile {
            label: "CYCPLUS BC2 virtual shifter".into(),
            name_prefixes: vec!["CYCPLUS BC2".into()],
            services: Vec::new(),
            // Nordic UART TX; 8-byte frames, byte 6 = up and byte 7 = down: 0x01 pressed, 0x03 idle.
            characteristic: "6e400003-b5a3-f393-e0a9-e50e24dcca9e".into(),
            shift_up_hex: String::new(),
            shift_down_hex: String::new(),
            buttons: Some(ButtonMap {
                shift_up: vec![byte_is(6, 0x01)],
                shift_down: vec![byte_is(7, 0x01)],
                ignore_first_frame: false,
            }),
            evidence: "protocol as in OpenBikeControl's BikeControl (cycplus_bc2)".into(),
        },
        ControllerProfile {
            label: "Elite Square (shift levers)".into(),
            name_prefixes: vec!["SQUARE".into()],
            services: vec!["347b0001-7635-408b-8918-8ff3949ce592".into()],
            // Bytes 3-6 are a button bitmask; shift 1 on either side is harder, shift 2 easier.
            characteristic: "347b0043-7635-408b-8918-8ff3949ce592".into(),
            shift_up_hex: String::new(),
            shift_down_hex: String::new(),
            buttons: Some(ButtonMap {
                shift_up: vec![bit(6, 0x02), bit(4, 0x02)],
                shift_down: vec![bit(6, 0x01), bit(4, 0x01)],
                ignore_first_frame: true,
            }),
            evidence: "protocol as in OpenBikeControl's BikeControl (elite_square)".into(),
        },
        ControllerProfile {
            label: "Shimano Di2 (D-Fly channel 1 harder, 2 easier)".into(),
            name_prefixes: vec!["RDR".into()],
            services: vec![
                "000018ef-5348-494d-414e-4f5f424c4500".into(),
                "000018ff-5348-494d-414e-4f5f424c4500".into(),
            ],
            // Indications: byte 0, then one byte per D-Fly channel; 0x10 short, 0x20 long and
            // 0x40 double press.
            characteristic: "00002ac2-5348-494d-414e-4f5f424c4500".into(),
            shift_up_hex: String::new(),
            shift_down_hex: String::new(),
            buttons: Some(ButtonMap {
                shift_up: vec![bit(1, 0x70)],
                shift_down: vec![bit(2, 0x70)],
                ignore_first_frame: true,
            }),
            evidence: "protocol as in OpenBikeControl's BikeControl (shimano_di2)".into(),
        },
    ]
}
/// Parse user profiles (a JSON array of [`ControllerProfile`]) and check each can decode.
pub fn parse_controllers(json: &str) -> Result<Vec<ControllerProfile>, String> {
    let profiles: Vec<ControllerProfile> =
        serde_json::from_str(json).map_err(|e| format!("invalid controller profiles: {e}"))?;
    for p in &profiles {
        p.check()?;
    }
    Ok(profiles)
}
/// The first profile matching the advertised name or services.
pub fn controller_profile<'a>(
    profiles: &'a [ControllerProfile],
    name: &str,
    services: &[String],
) -> Option<&'a ControllerProfile> {
    profiles.iter().find(|p| p.matches(name, services))
}

/// A name prefix for a learned controller: the advertised name without its trailing serial
/// ("Think VS01-0001234" gives "Think VS").
pub fn name_prefix(name: &str) -> String {
    let trimmed = name
        .trim_end_matches(|c: char| c.is_ascii_digit() || " -_:#".contains(c))
        .to_owned();
    if trimmed.trim().is_empty() {
        name.to_owned()
    } else {
        trimmed
    }
}

/// Messages received while the user pressed one button several times: (characteristic, bytes).
pub type Presses = [(String, Vec<u8>)];

/// Build a click-frame profile from presses of the harder (`up`) and easier (`down`) buttons:
/// on one characteristic, a message repeated during one step and never sent in the other.
/// Controllers that change every message (counters, encryption) cannot be learned this way.
pub fn learn_profile(
    label: &str,
    name: &str,
    up: &Presses,
    down: &Presses,
) -> Result<ControllerProfile, String> {
    if up.is_empty() || down.is_empty() {
        let step = if up.is_empty() { "harder" } else { "easier" };
        return Err(format!(
            "Nothing arrived while pressing the {step} button. Is this the right device?"
        ));
    }
    let mut found = None;
    let mut characteristics: Vec<&str> = up.iter().map(|(c, _)| c.as_str()).collect();
    characteristics.sort_unstable();
    characteristics.dedup();
    for c in characteristics {
        if let (Some((u, nu)), Some((d, nd))) = (repeated(c, up, down), repeated(c, down, up)) {
            // Prefer the characteristic whose frames repeated most reliably.
            if found.as_ref().is_none_or(|(_, _, _, n)| nu.min(nd) > *n) {
                found = Some((c, u, d, nu.min(nd)));
            }
        }
    }
    let Some((characteristic, up, down, _)) = found else {
        return Err(
            "No message repeated with each press. Press each button at least three \
             times, one press at a time. If it still fails, this controller changes its messages \
             with every press and needs built-in support."
                .into(),
        );
    };
    let profile = ControllerProfile {
        label: label.trim().to_owned(),
        name_prefixes: vec![name_prefix(name)],
        services: Vec::new(),
        characteristic: characteristic.to_owned(),
        shift_up_hex: bike_protocol::hex(up),
        shift_down_hex: bike_protocol::hex(down),
        buttons: None,
        evidence: format!("learned in {APP_NAME} from {name}"),
    };
    profile.check()?;
    Ok(profile)
}

/// The most repeated message on `c` in `this` step that never occurs in `other`.
fn repeated<'a>(c: &str, this: &'a Presses, other: &Presses) -> Option<(&'a [u8], usize)> {
    let mut counts: Vec<(&[u8], usize)> = Vec::new();
    for (_, bytes) in this.iter().filter(|(ch, b)| ch == c && !b.is_empty()) {
        if other.iter().any(|(ch, b)| ch == c && b == bytes) {
            continue;
        }
        match counts.iter_mut().find(|(b, _)| *b == bytes.as_slice()) {
            Some((_, n)) => *n += 1,
            None => counts.push((bytes, 1)),
        }
    }
    counts
        .into_iter()
        .filter(|&(_, n)| n >= 2)
        .max_by_key(|&(_, n)| n)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerMap {
    pub characteristic: String,
    pub shift_up_hex: String,
    pub shift_down_hex: String,
    /// Capture-backed documentation reference, required to avoid implicit VS200 assumptions.
    pub evidence: String,
}
/// Decodes one controller's messages. Button-state decoding remembers which buttons are down,
/// so use a fresh clone per connection.
#[derive(Debug, Clone)]
pub struct PacketMap {
    pub characteristic: String,
    decoder: Decoder,
}
#[derive(Debug, Clone)]
enum Decoder {
    Frames {
        up: Vec<u8>,
        down: Vec<u8>,
    },
    Buttons {
        up: Vec<Bits>,
        down: Vec<Bits>,
        ignore_first: bool,
        seen: bool,
    },
}
#[derive(Debug, Clone)]
struct Bits {
    mask: Vec<u8>,
    value: Option<Vec<u8>>,
    held: bool,
}
impl Bits {
    fn new(b: &ButtonBits) -> Result<Self, String> {
        let mask = bike_protocol::unhex(&b.mask).map_err(|e| e.to_string())?;
        if mask.iter().all(|&m| m == 0) {
            return Err("button mask must have a bit set".into());
        }
        let value = b
            .value
            .as_deref()
            .map(bike_protocol::unhex)
            .transpose()
            .map_err(|e| e.to_string())?;
        if value.as_ref().is_some_and(|v| v.len() != mask.len()) {
            return Err("button value must be as long as its mask".into());
        }
        Ok(Self {
            mask,
            value,
            held: false,
        })
    }
    fn pressed(&self, bytes: &[u8]) -> bool {
        if bytes.len() < self.mask.len() {
            return false;
        }
        let mut masked = bytes.iter().zip(&self.mask).map(|(b, m)| b & m);
        match &self.value {
            Some(v) => masked.eq(v.iter().copied()),
            None => masked.any(|b| b != 0),
        }
    }
}
impl PacketMap {
    /// Subscribe for capture only; no byte sequence can trigger an action.
    pub fn capture(characteristic: String) -> Self {
        Self {
            characteristic,
            decoder: Decoder::Frames {
                up: Vec::new(),
                down: Vec::new(),
            },
        }
    }
    pub fn new(config: ControllerMap) -> Result<Self, String> {
        if config.evidence.trim().is_empty() {
            return Err("mapping requires an evidence reference".into());
        }
        let up = bike_protocol::unhex(&config.shift_up_hex).map_err(|e| e.to_string())?;
        let down = bike_protocol::unhex(&config.shift_down_hex).map_err(|e| e.to_string())?;
        if up.is_empty() || down.is_empty() || up == down {
            return Err("up/down patterns must be distinct and nonempty".into());
        }
        Ok(Self {
            characteristic: config.characteristic,
            decoder: Decoder::Frames { up, down },
        })
    }
    /// Click frames: each matching packet is one click. Button states: one click per button
    /// that went down in this packet (up wins if both did).
    pub fn decode(&mut self, bytes: &[u8]) -> Option<Action> {
        match &mut self.decoder {
            Decoder::Frames { up, down } => {
                if !up.is_empty() && bytes == up.as_slice() {
                    Some(Action::ShiftUp)
                } else if !down.is_empty() && bytes == down.as_slice() {
                    Some(Action::ShiftDown)
                } else {
                    None
                }
            }
            Decoder::Buttons {
                up,
                down,
                ignore_first,
                seen,
            } => {
                let resting = *ignore_first && !*seen;
                *seen = true;
                let mut action = None;
                let buttons = up
                    .iter_mut()
                    .map(|b| (b, Action::ShiftUp))
                    .chain(down.iter_mut().map(|b| (b, Action::ShiftDown)));
                for (button, a) in buttons {
                    let now = button.pressed(bytes);
                    if now && !button.held && !resting && action.is_none() {
                        action = Some(a);
                    }
                    button.held = now;
                }
                action
            }
        }
    }
}
#[derive(Debug, Default)]
pub struct RetryBudget {
    failures: u32,
}
impl RetryBudget {
    pub fn next_delay(&mut self) -> Option<Duration> {
        if self.failures >= 5 {
            return None;
        }
        let seconds = 1u64 << self.failures;
        self.failures += 1;
        Some(Duration::from_secs(seconds))
    }
    /// Reset only after a stable connected period, not an immediate reconnect/flap.
    pub fn stable(&mut self) {
        self.failures = 0;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_retries() {
        let mut b = RetryBudget::default();
        let mut got = vec![];
        while let Some(d) = b.next_delay() {
            got.push(d.as_secs());
        }
        assert_eq!(got, vec![1, 2, 4, 8, 16]);
        b.stable();
        assert_eq!(b.next_delay(), Some(Duration::from_secs(1)));
    }
    #[test]
    fn map_is_explicit() {
        let c = ControllerMap {
            characteristic: "test".into(),
            shift_up_hex: "01".into(),
            shift_down_hex: "01".into(),
            evidence: "synthetic fixture".into(),
        };
        assert!(PacketMap::new(c).is_err());
    }
    #[test]
    fn capture_never_generates_actions() {
        let mut m = PacketMap::capture("synthetic".into());
        assert_eq!(m.decode(&[]), None);
        assert_eq!(m.decode(&[1]), None);
    }
    #[test]
    fn mapping_preserves_distinct_clicks_and_ignores_unknown() {
        let c = ControllerMap {
            characteristic: "synthetic".into(),
            shift_up_hex: "aabb".into(),
            shift_down_hex: "ccdd".into(),
            evidence: "synthetic test".into(),
        };
        let Ok(mut m) = PacketMap::new(c) else {
            panic!("valid mapping rejected")
        };
        assert_eq!(m.decode(&[0xaa, 0xbb]), Some(Action::ShiftUp));
        assert_eq!(m.decode(&[0xaa, 0xbb]), Some(Action::ShiftUp));
        assert_eq!(m.decode(&[0xcc, 0xdd]), Some(Action::ShiftDown));
        assert_eq!(m.decode(&[0xaa]), None);
    }
    #[test]
    fn builtin_profiles_are_valid_and_matched_by_prefix() {
        let builtin = builtin_controllers();
        for p in &builtin {
            assert!(p.packet_map().is_ok(), "{}", p.label);
        }
        let Some(v100) = controller_profile(&builtin, "Think VS01-0001234", &[]) else {
            panic!("V100 not matched")
        };
        assert!(v100.label.contains("V100") && v100.label.contains("VS200"));
        // The VS200 advertises the same name in capitals.
        assert_eq!(
            controller_profile(&builtin, "THINK VS01-0000285", &[]).map(|p| &p.label),
            Some(&v100.label)
        );
        assert!(controller_profile(&builtin, "Think X222- 0001234", &[]).is_none());
        let Ok(mut m) = v100.packet_map() else {
            panic!("invalid V100 map")
        };
        assert_eq!(
            m.decode(&[0xf3, 0x05, 0x03, 0x00, 0xfb]),
            Some(Action::ShiftDown)
        );
    }
    #[test]
    fn user_profiles_parse_and_match_any_vendor() {
        let json = r#"[{"label": "Acme Shifter", "name_prefixes": ["ACME-"],
            "characteristic": "0000aaaa-0000-1000-8000-00805f9b34fb",
            "shift_up_hex": "0101", "shift_down_hex": "0102"}]"#;
        let Ok(profiles) = parse_controllers(json) else {
            panic!("valid profile rejected")
        };
        let Some(p) = controller_profile(&profiles, "ACME-42", &[]) else {
            panic!("user profile not matched")
        };
        let Ok(mut m) = p.packet_map() else {
            panic!("user profile map rejected")
        };
        assert_eq!(m.decode(&[0x01, 0x01]), Some(Action::ShiftUp));
        assert!(controller_profile(&profiles, "Other", &[]).is_none());
        let same_frames = json.replace("0102", "0101");
        assert!(parse_controllers(&same_frames).is_err());
        let no_prefix = json.replace(r#"["ACME-"]"#, "[]");
        assert!(parse_controllers(&no_prefix).is_err());
        assert!(parse_controllers(r#"[{"label": "x", "typo": 1}]"#).is_err());
    }
    #[test]
    fn captured_v100_clicks() {
        // Real FEA1 frames, V100 fw V1.0.5; see tests/fixtures/README.md.
        let c = ControllerMap {
            characteristic: "0000fea1-0000-1000-8000-00805f9b34fb".into(),
            shift_up_hex: "f3050300fb".into(),
            shift_down_hex: "f3050301fc".into(),
            evidence: "ride2-v100-fea1 capture".into(),
        };
        let Ok(mut m) = PacketMap::new(c) else {
            panic!("captured mapping rejected")
        };
        assert_eq!(
            m.decode(&[0xf3, 0x05, 0x03, 0x00, 0xfb]),
            Some(Action::ShiftUp)
        );
        assert_eq!(
            m.decode(&[0xf3, 0x05, 0x03, 0x01, 0xfc]),
            Some(Action::ShiftDown)
        );
        assert_eq!(m.decode(&[0xf3, 0x05, 0x03, 0x02, 0xfd]), None);
        assert_eq!(m.decode(&[0xf3, 0x05, 0x03, 0x00]), None);
    }

    fn builtin(label: &str) -> PacketMap {
        let Some(p) = builtin_controllers()
            .into_iter()
            .find(|p| p.label.starts_with(label))
        else {
            panic!("{label} not built in")
        };
        let Ok(m) = p.packet_map() else {
            panic!("{label} map rejected")
        };
        m
    }
    fn decode_all(m: &mut PacketMap, frames: &[&str]) -> Vec<Option<Action>> {
        frames
            .iter()
            .map(|f| m.decode(&bike_protocol::unhex(f).unwrap_or_default()))
            .collect()
    }
    #[test]
    fn cycplus_presses_are_button_downs() {
        let mut m = builtin("CYCPLUS");
        let (up, down) = (Some(Action::ShiftUp), Some(Action::ShiftDown));
        // Idle 03, pressed 01; byte 6 is up, byte 7 down. A held button reports again.
        let got = decode_all(
            &mut m,
            &[
                "feefffee02060103",
                "feefffee02060103",
                "feefffee02060303",
                "feefffee02060301",
                "feefffee02060303",
                "feefffee0206", // truncated: ignored
            ],
        );
        assert_eq!(got, vec![up, None, None, down, None, None]);
    }
    #[test]
    fn elite_square_ignores_resting_frame_and_maps_both_sides() {
        let mut m = builtin("Elite Square");
        let frame = |code: &str| format!("030153{code}0318f40101");
        let frames: Vec<String> = [
            "00000002", // already held when connecting: resting state
            "00000000", "00000002", // left shift 1
            "00000000", "00020000", // right shift 1
            "00010000", // right shift 2 replaces it
            "00000100", // d-pad left: not a shift
        ]
        .iter()
        .map(|c| frame(c))
        .collect();
        let refs: Vec<&str> = frames.iter().map(String::as_str).collect();
        assert_eq!(
            decode_all(&mut m, &refs),
            vec![
                None,
                None,
                Some(Action::ShiftUp),
                None,
                Some(Action::ShiftUp),
                Some(Action::ShiftDown),
                None
            ]
        );
    }
    #[test]
    fn di2_long_press_is_one_click_and_found_by_service() {
        let profiles = builtin_controllers();
        let service = "000018EF-5348-494D-414E-4F5F424C4500".to_owned();
        assert!(
            controller_profile(&profiles, "", &[service])
                .is_some_and(|p| p.label.starts_with("Shimano Di2"))
        );
        let mut m = builtin("Shimano Di2");
        let got = decode_all(
            &mut m,
            &[
                "00100000", // stale state from before the reconnect
                "00000000", "00100000", // channel 1 short press
                "00000000", "00200000", // channel 1 long press, then short while still held
                "00100000", "00000000", "00004000", // channel 2 double press
                "00000010", // channel 3: unmapped
            ],
        );
        assert_eq!(
            got,
            vec![
                None,
                None,
                Some(Action::ShiftUp),
                None,
                Some(Action::ShiftUp),
                None,
                None,
                Some(Action::ShiftDown),
                None
            ]
        );
    }
    #[test]
    fn user_button_profiles_are_checked() {
        let json = r#"[{"label": "Bits", "name_prefixes": ["B"], "characteristic": "x",
            "buttons": {"shift_up": [{"mask": "ffff80", "value": "000180"}],
                        "shift_down": [{"mask": "ffff80", "value": "800080"}]}}]"#;
        let Ok(profiles) = parse_controllers(json) else {
            panic!("valid button profile rejected")
        };
        let Ok(mut m) = profiles[0].packet_map() else {
            panic!("button map rejected")
        };
        // Pressed only with the top bit of byte 2; the low bits are a rolling counter.
        assert_eq!(
            decode_all(&mut m, &["0001e6", "000166", "0001e7", "80005e"]),
            vec![Some(Action::ShiftUp), None, Some(Action::ShiftUp), None]
        );
        assert_eq!(m.decode(&[0x80, 0x00, 0xdf]), Some(Action::ShiftDown));
        let both = json.replace(r#""buttons""#, r#""shift_up_hex": "01", "buttons""#);
        assert!(parse_controllers(&both).is_err());
        let short_value = json.replace(r#""value": "000180""#, r#""value": "01""#);
        assert!(parse_controllers(&short_value).is_err());
        let zero_mask = json.replace(r#""mask": "ffff80", "value": "000180""#, r#""mask": "00""#);
        assert!(parse_controllers(&zero_mask).is_err());
        let no_down = json.replace(r#"{"mask": "ffff80", "value": "800080"}"#, "");
        assert!(parse_controllers(&no_down).is_err());
    }
    #[test]
    fn learned_names_drop_the_serial() {
        assert_eq!(name_prefix("Think VS01-0001234"), "Think VS");
        assert_eq!(name_prefix("ACME Shifter 42"), "ACME Shifter");
        assert_eq!(name_prefix("Remote"), "Remote");
        assert_eq!(name_prefix("12345"), "12345");
    }
    fn presses(frames: &[(&str, &str)]) -> Vec<(String, Vec<u8>)> {
        frames
            .iter()
            .map(|(c, f)| ((*c).to_owned(), bike_protocol::unhex(f).unwrap_or_default()))
            .collect()
    }
    #[test]
    fn learning_finds_the_click_frames() {
        let (buttons, battery) = ("0000fea1", "00002a19");
        let up = presses(&[
            (battery, "5d"),
            (buttons, "f3050301fc"),
            (buttons, "f3050301fc"),
            (buttons, "f3050301fc"),
        ]);
        let down = presses(&[
            (buttons, "f3050300fb"),
            (battery, "5d"),
            (buttons, "f3050300fb"),
            (buttons, "f3050300fb"),
        ]);
        let Ok(p) = learn_profile(" My shifter ", "Think VS01-0001234", &up, &down) else {
            panic!("clean presses not learned")
        };
        assert_eq!(p.label, "My shifter");
        assert_eq!(p.name_prefixes, vec!["Think VS".to_owned()]);
        assert_eq!(p.characteristic, buttons);
        assert_eq!(
            (p.shift_up_hex.as_str(), p.shift_down_hex.as_str()),
            ("f3050301fc", "f3050300fb")
        );
        // The result round-trips through controllers.json.
        let json = serde_json::to_string(&vec![p.clone()]).unwrap_or_default();
        assert_eq!(parse_controllers(&json), Ok(vec![p]));
    }
    #[test]
    fn learning_press_release_controllers_uses_the_press_frame() {
        let (rx, idle) = ("6e400003", "feefffee02060303");
        let up = presses(&[
            (rx, "feefffee02060103"),
            (rx, idle),
            (rx, "feefffee02060103"),
            (rx, idle),
        ]);
        let down = presses(&[
            (rx, "feefffee02060301"),
            (rx, idle),
            (rx, "feefffee02060301"),
            (rx, idle),
        ]);
        let Ok(p) = learn_profile("BC2", "CYCPLUS BC2", &up, &down) else {
            panic!("press/release controller not learned")
        };
        assert_eq!(p.shift_up_hex, "feefffee02060103");
        assert_eq!(p.shift_down_hex, "feefffee02060301");
    }
    #[test]
    fn learning_rejects_changing_or_missing_messages() {
        let c = "0000aaaa";
        let counter_up = presses(&[(c, "0001e6"), (c, "0001e7"), (c, "0001e8")]);
        let counter_down = presses(&[(c, "8000e9"), (c, "8000ea"), (c, "8000eb")]);
        assert!(learn_profile("x", "X", &counter_up, &counter_down).is_err());
        assert!(learn_profile("x", "X", &[], &counter_down).is_err());
        let up = presses(&[(c, "01"), (c, "01")]);
        let down = presses(&[(c, "02"), (c, "02")]);
        assert!(learn_profile("x", "X", &up, &down).is_ok());
        assert!(learn_profile(" ", "X", &up, &down).is_err());
    }
    #[test]
    fn manual_examples_are_valid() {
        let manual = include_str!("../../../docs/MANUAL.md");
        let examples: Vec<&str> = manual
            .split("```json")
            .skip(1)
            .filter_map(|block| block.split("```").next())
            .collect();
        assert_eq!(examples.len(), 2);
        for json in &examples {
            assert!(parse_controllers(json).is_ok(), "{json}");
        }
        let Ok(di2) = parse_controllers(examples[1]) else {
            panic!("Di2 example rejected")
        };
        let Ok(mut m) = di2[0].packet_map() else {
            panic!("Di2 example map rejected")
        };
        assert_eq!(
            decode_all(
                &mut m,
                &["0000000000", "0000001000", "0000000000", "0000000020"]
            ),
            vec![None, Some(Action::ShiftUp), None, Some(Action::ShiftDown)]
        );
    }
}
