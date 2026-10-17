//! DirCon framing: 6-byte header (version 1, id, sequence, response code, big-endian length).
use thiserror::Error;

pub const DISCOVER_SERVICES: u8 = 0x01;
pub const DISCOVER_CHARACTERISTICS: u8 = 0x02;
pub const READ: u8 = 0x03;
pub const WRITE: u8 = 0x04;
pub const ENABLE_NOTIFICATIONS: u8 = 0x05;
pub const NOTIFICATION: u8 = 0x06;
pub const UNKNOWN_07: u8 = 0x07;

pub const SUCCESS: u8 = 0x00;
pub const UNKNOWN_MESSAGE: u8 = 0x01;
pub const UNEXPECTED_ERROR: u8 = 0x02;
pub const SERVICE_NOT_FOUND: u8 = 0x03;
pub const CHARACTERISTIC_NOT_FOUND: u8 = 0x04;
pub const NOT_SUPPORTED: u8 = 0x05;

const HEADER: usize = 6;
/// Largest buffered input before the client is considered broken.
const MAX_BUFFER: usize = 16 * 1024;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CodecError {
    #[error("unsupported DirCon version {0}")]
    Version(u8),
    #[error("DirCon input exceeds {MAX_BUFFER} bytes")]
    Overflow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub id: u8,
    pub seq: u8,
    pub code: u8,
    pub payload: Vec<u8>,
}

impl Packet {
    pub fn encode(&self) -> Vec<u8> {
        // Payloads are built from bounded GATT data; 64 KiB cannot be reached.
        let len = u16::try_from(self.payload.len()).unwrap_or(u16::MAX);
        let mut out = Vec::with_capacity(HEADER + self.payload.len());
        out.extend_from_slice(&[1, self.id, self.seq, self.code]);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&self.payload);
        out
    }
}

/// Accumulates TCP bytes; TCP reads are not message boundaries.
#[derive(Debug, Default)]
pub struct Decoder {
    buffer: Vec<u8>,
}

impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Packet>, CodecError> {
        if self.buffer.len() + bytes.len() > MAX_BUFFER {
            return Err(CodecError::Overflow);
        }
        self.buffer.extend_from_slice(bytes);
        let mut out = Vec::new();
        while self.buffer.len() >= HEADER {
            if self.buffer[0] != 1 {
                return Err(CodecError::Version(self.buffer[0]));
            }
            let len = usize::from(u16::from_be_bytes([self.buffer[4], self.buffer[5]]));
            if self.buffer.len() < HEADER + len {
                break;
            }
            let frame: Vec<u8> = self.buffer.drain(..HEADER + len).collect();
            out.push(Packet {
                id: frame[1],
                seq: frame[2],
                code: frame[3],
                payload: frame[HEADER..].to_vec(),
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_fragmented_and_coalesced() {
        let a = Packet {
            id: READ,
            seq: 7,
            code: SUCCESS,
            payload: vec![0xaa; 16],
        };
        let b = Packet {
            id: DISCOVER_SERVICES,
            seq: 8,
            code: SUCCESS,
            payload: vec![],
        };
        let mut wire = a.encode();
        wire.extend(b.encode());
        assert_eq!(&wire[..6], &[1, READ, 7, 0, 0, 16]);
        let mut d = Decoder::default();
        assert_eq!(d.push(&wire[..5]), Ok(vec![]));
        assert_eq!(d.push(&wire[5..20]), Ok(vec![]));
        assert_eq!(d.push(&wire[20..]), Ok(vec![a, b]));
    }

    #[test]
    fn rejects_bad_version_and_unbounded_input() {
        assert_eq!(
            Decoder::default().push(&[2, 1, 0, 0, 0, 0]),
            Err(CodecError::Version(2))
        );
        assert_eq!(
            Decoder::default().push(&vec![1; MAX_BUFFER + 1]),
            Err(CodecError::Overflow)
        );
    }
}
