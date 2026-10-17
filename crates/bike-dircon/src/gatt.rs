//! The virtual GATT table served over DirCon and the per-request logic. Pure, no I/O.
use crate::codec::{self, Packet};
use std::collections::HashSet;
use uuid::Uuid;

pub const READ: u8 = 0x01;
pub const WRITE: u8 = 0x02;
pub const NOTIFY: u8 = 0x04;
pub const INDICATE: u8 = 0x08;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Characteristic {
    pub uuid: Uuid,
    pub properties: u8,
    /// Value returned to reads; only meaningful with `READ`.
    pub value: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub uuid: Uuid,
    pub characteristics: Vec<Characteristic>,
}

/// A 16-bit Bluetooth SIG UUID in full form.
pub const fn sig(short: u16) -> Uuid {
    Uuid::from_u128(((short as u128) << 96) | 0x0000_0000_0000_1000_8000_0080_5f9b_34fb)
}

#[derive(Debug, Clone, Default)]
pub struct Gatt {
    pub services: Vec<Service>,
}

/// What the server must do after answering a request.
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    /// The client wrote to a writable characteristic.
    Write(Uuid, Vec<u8>),
}

fn uuid_at(payload: &[u8]) -> Option<Uuid> {
    Some(Uuid::from_bytes(payload.get(..16)?.try_into().ok()?))
}

impl Gatt {
    fn characteristic(&self, uuid: Uuid) -> Option<&Characteristic> {
        self.services
            .iter()
            .flat_map(|s| &s.characteristics)
            .find(|c| c.uuid == uuid)
    }

    /// Answer one client request. `subscriptions` is the client's notification set.
    pub fn handle(&self, request: &Packet, subscriptions: &mut HashSet<Uuid>) -> (Packet, Effect) {
        let reply = |code: u8, payload: Vec<u8>| Packet {
            id: request.id,
            seq: request.seq,
            code,
            payload,
        };
        let p = &request.payload;
        match request.id {
            codec::DISCOVER_SERVICES => {
                let payload = self
                    .services
                    .iter()
                    .flat_map(|s| *s.uuid.as_bytes())
                    .collect();
                (reply(codec::SUCCESS, payload), Effect::None)
            }
            codec::DISCOVER_CHARACTERISTICS => {
                let Some(service) =
                    uuid_at(p).and_then(|u| self.services.iter().find(|s| s.uuid == u))
                else {
                    return (reply(codec::SERVICE_NOT_FOUND, p.clone()), Effect::None);
                };
                let mut payload = service.uuid.as_bytes().to_vec();
                for c in &service.characteristics {
                    payload.extend_from_slice(c.uuid.as_bytes());
                    payload.push(c.properties);
                }
                (reply(codec::SUCCESS, payload), Effect::None)
            }
            codec::READ | codec::WRITE | codec::ENABLE_NOTIFICATIONS => {
                let Some(uuid) = uuid_at(p) else {
                    return (reply(codec::UNEXPECTED_ERROR, vec![]), Effect::None);
                };
                let Some(c) = self.characteristic(uuid) else {
                    return (
                        reply(codec::CHARACTERISTIC_NOT_FOUND, uuid.as_bytes().to_vec()),
                        Effect::None,
                    );
                };
                let echo = uuid.as_bytes().to_vec();
                match request.id {
                    codec::READ if c.properties & READ != 0 => {
                        let mut payload = echo;
                        payload.extend_from_slice(&c.value);
                        (reply(codec::SUCCESS, payload), Effect::None)
                    }
                    codec::WRITE if c.properties & WRITE != 0 && p.len() > 16 => (
                        reply(codec::SUCCESS, echo),
                        Effect::Write(uuid, p[16..].to_vec()),
                    ),
                    codec::ENABLE_NOTIFICATIONS
                        if c.properties & (NOTIFY | INDICATE) != 0 && p.len() == 17 =>
                    {
                        if p[16] == 0 {
                            subscriptions.remove(&uuid);
                        } else {
                            subscriptions.insert(uuid);
                        }
                        (reply(codec::SUCCESS, echo), Effect::None)
                    }
                    _ => (reply(codec::NOT_SUPPORTED, echo), Effect::None),
                }
            }
            codec::UNKNOWN_07 => (reply(codec::SUCCESS, vec![]), Effect::None),
            _ => (reply(codec::UNKNOWN_MESSAGE, vec![]), Effect::None),
        }
    }
}

/// Unsolicited notification for a subscribed characteristic (sequence 0).
pub fn notification(uuid: Uuid, value: &[u8]) -> Packet {
    let mut payload = uuid.as_bytes().to_vec();
    payload.extend_from_slice(value);
    Packet {
        id: codec::NOTIFICATION,
        seq: 0,
        code: codec::SUCCESS,
        payload,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gatt() -> Gatt {
        Gatt {
            services: vec![Service {
                uuid: sig(0x1826),
                characteristics: vec![
                    Characteristic {
                        uuid: sig(0x2acc),
                        properties: READ,
                        value: vec![3, 0x40, 0, 0, 0x0c, 0xa0, 0, 0],
                    },
                    Characteristic {
                        uuid: sig(0x2ad2),
                        properties: NOTIFY,
                        value: vec![],
                    },
                    Characteristic {
                        uuid: sig(0x2ad9),
                        properties: WRITE | INDICATE,
                        value: vec![],
                    },
                ],
            }],
        }
    }
    fn req(id: u8, payload: Vec<u8>) -> Packet {
        Packet {
            id,
            seq: 42,
            code: 0,
            payload,
        }
    }
    fn with(u: Uuid, extra: &[u8]) -> Vec<u8> {
        let mut v = u.as_bytes().to_vec();
        v.extend_from_slice(extra);
        v
    }

    #[test]
    fn sig_uuid_form() {
        assert_eq!(
            sig(0x1826).to_string(),
            "00001826-0000-1000-8000-00805f9b34fb"
        );
    }

    #[test]
    fn discovery_and_read() {
        let g = gatt();
        let mut subs = HashSet::new();
        let (r, _) = g.handle(&req(codec::DISCOVER_SERVICES, vec![]), &mut subs);
        assert_eq!(
            (r.seq, r.code, r.payload),
            (42, 0, sig(0x1826).as_bytes().to_vec())
        );
        let (r, _) = g.handle(
            &req(codec::DISCOVER_CHARACTERISTICS, with(sig(0x1826), &[])),
            &mut subs,
        );
        assert_eq!(r.payload.len(), 16 + 3 * 17);
        assert_eq!(r.payload[16 + 17 + 16], NOTIFY);
        let (r, _) = g.handle(&req(codec::READ, with(sig(0x2acc), &[])), &mut subs);
        assert_eq!(&r.payload[16..], &[3, 0x40, 0, 0, 0x0c, 0xa0, 0, 0]);
        let (r, _) = g.handle(&req(codec::READ, with(sig(0x2ad2), &[])), &mut subs);
        assert_eq!(r.code, codec::NOT_SUPPORTED);
        let (r, _) = g.handle(&req(codec::READ, with(sig(0x2a00), &[])), &mut subs);
        assert_eq!(r.code, codec::CHARACTERISTIC_NOT_FOUND);
        let (r, _) = g.handle(
            &req(codec::DISCOVER_CHARACTERISTICS, with(sig(0x1818), &[])),
            &mut subs,
        );
        assert_eq!(r.code, codec::SERVICE_NOT_FOUND);
    }

    #[test]
    fn subscribe_and_write() {
        let g = gatt();
        let mut subs = HashSet::new();
        let (r, _) = g.handle(
            &req(codec::ENABLE_NOTIFICATIONS, with(sig(0x2ad2), &[1])),
            &mut subs,
        );
        assert_eq!(r.code, 0);
        assert!(subs.contains(&sig(0x2ad2)));
        g.handle(
            &req(codec::ENABLE_NOTIFICATIONS, with(sig(0x2ad2), &[0])),
            &mut subs,
        );
        assert!(subs.is_empty());
        let (r, e) = g.handle(&req(codec::WRITE, with(sig(0x2ad9), &[0x00])), &mut subs);
        assert_eq!((r.code, e), (0, Effect::Write(sig(0x2ad9), vec![0x00])));
        let (r, e) = g.handle(&req(codec::WRITE, with(sig(0x2acc), &[1])), &mut subs);
        assert_eq!((r.code, e), (codec::NOT_SUPPORTED, Effect::None));
        let (r, _) = g.handle(&req(codec::READ, vec![1, 2]), &mut subs);
        assert_eq!(r.code, codec::UNEXPECTED_ERROR);
    }

    #[test]
    fn notification_shape() {
        let n = notification(sig(0x2ad2), &[0x44, 0]);
        assert_eq!(n.encode()[..6], [1, codec::NOTIFICATION, 0, 0, 0, 18]);
    }
}
