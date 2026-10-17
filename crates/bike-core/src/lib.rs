//! Transport-independent lifecycle and exact packet mapping.
use bike_protocol::Action;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// User-visible product name (also advertised to MyWhoosh). The command stays `bikectl`.
pub const APP_NAME: &str = "OsmolControl";

/// A known controller: how to recognise it in a scan and how to decode its button frames.
/// Built-in profiles come from [`builtin_controllers`]; users add more as JSON (same fields).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerProfile {
    pub label: String,
    /// Advertised local-name prefixes; vendor names often end in a per-unit serial.
    pub name_prefixes: Vec<String>,
    pub characteristic: String,
    pub shift_up_hex: String,
    pub shift_down_hex: String,
    /// Where the frames come from (a capture, a document); optional for user profiles.
    #[serde(default)]
    pub evidence: String,
}
impl ControllerProfile {
    pub fn matches(&self, name: &str) -> bool {
        self.name_prefixes
            .iter()
            .any(|p| !p.is_empty() && name.starts_with(p.as_str()))
    }
    pub fn packet_map(&self) -> Result<PacketMap, String> {
        let evidence = if self.evidence.trim().is_empty() {
            format!("user profile {}", self.label)
        } else {
            self.evidence.clone()
        };
        PacketMap::new(ControllerMap {
            characteristic: self.characteristic.clone(),
            shift_up_hex: self.shift_up_hex.clone(),
            shift_down_hex: self.shift_down_hex.clone(),
            evidence,
        })
    }
}
/// Built-in controllers. Add an entry only from a labelled capture (`bikectl capture`).
pub fn builtin_controllers() -> Vec<ControllerProfile> {
    vec![ControllerProfile {
        label: "ThinkRider V100".into(),
        name_prefixes: vec!["Think VS01".into()],
        characteristic: "0000fea1-0000-1000-8000-00805f9b34fb".into(),
        // The button printed '+' sends f3050301fc (shift up, harder gear).
        shift_up_hex: "f3050301fc".into(),
        shift_down_hex: "f3050300fb".into(),
        evidence: "docs/devices/v100.md (captures, fw V1.0.5)".into(),
    }]
}
/// Parse user profiles (a JSON array of [`ControllerProfile`]) and check each can decode.
pub fn parse_controllers(json: &str) -> Result<Vec<ControllerProfile>, String> {
    let profiles: Vec<ControllerProfile> =
        serde_json::from_str(json).map_err(|e| format!("invalid controller profiles: {e}"))?;
    for p in &profiles {
        if p.name_prefixes.iter().all(|n| n.is_empty()) {
            return Err(format!("{}: name_prefixes must not be empty", p.label));
        }
        p.packet_map().map_err(|e| format!("{}: {e}", p.label))?;
    }
    Ok(profiles)
}
/// The first profile whose name prefix matches the advertised name.
pub fn controller_profile<'a>(
    profiles: &'a [ControllerProfile],
    name: &str,
) -> Option<&'a ControllerProfile> {
    profiles.iter().find(|p| p.matches(name))
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
#[derive(Debug, Clone)]
pub struct PacketMap {
    pub characteristic: String,
    up: Vec<u8>,
    down: Vec<u8>,
}
impl PacketMap {
    /// Subscribe for capture only; no byte sequence can trigger an action.
    pub fn capture(characteristic: String) -> Self {
        Self {
            characteristic,
            up: Vec::new(),
            down: Vec::new(),
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
            up,
            down,
        })
    }
    /// Each matching packet is one click. Hold/release protocols require a different explicit driver.
    pub fn decode(&self, bytes: &[u8]) -> Option<Action> {
        if !self.up.is_empty() && bytes == self.up {
            Some(Action::ShiftUp)
        } else if !self.down.is_empty() && bytes == self.down {
            Some(Action::ShiftDown)
        } else {
            None
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
        let m = PacketMap::capture("synthetic".into());
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
        let Ok(m) = PacketMap::new(c) else {
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
        let Some(v100) = controller_profile(&builtin, "Think VS01-0001234") else {
            panic!("V100 not matched")
        };
        assert_eq!(v100.label, "ThinkRider V100");
        assert!(controller_profile(&builtin, "Think X222- 0001234").is_none());
        let Ok(m) = v100.packet_map() else {
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
        let Some(p) = controller_profile(&profiles, "ACME-42") else {
            panic!("user profile not matched")
        };
        let Ok(m) = p.packet_map() else {
            panic!("user profile map rejected")
        };
        assert_eq!(m.decode(&[0x01, 0x01]), Some(Action::ShiftUp));
        assert!(controller_profile(&profiles, "Other").is_none());
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
        let Ok(m) = PacketMap::new(c) else {
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
}
