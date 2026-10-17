//! FTMS Control Point (2AD9) safety filter for commands relayed to a real trainer.
//!
//! Only the commands a training app needs are allowed, and targets are clamped to the
//! trainer's advertised ranges. Everything else is answered with the standard FTMS
//! "op code not supported" response instead of reaching the trainer.
use std::collections::VecDeque;

pub const REQUEST_CONTROL: u8 = 0x00;
pub const RESET: u8 = 0x01;
pub const SET_TARGET_RESISTANCE: u8 = 0x04;
pub const SET_TARGET_POWER: u8 = 0x05;
pub const START_OR_RESUME: u8 = 0x07;
pub const STOP_OR_PAUSE: u8 = 0x08;
pub const SET_SIMULATION: u8 = 0x11;
pub const RESPONSE: u8 = 0x80;

pub const RESULT_SUCCESS: u8 = 0x01;
pub const RESULT_NOT_SUPPORTED: u8 = 0x02;
pub const RESULT_INVALID_PARAMETER: u8 = 0x03;

/// Simulation grade limit, in 0.01 % units (±30 %).
const MAX_GRADE: i16 = 3000;

/// Trainer limits read from Supported Power Range (2AD8) and Resistance Range (2AD6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub power_w: (i16, i16),
    /// Resistance in the trainer's 0.1 units.
    pub resistance: (i16, i16),
}

impl Limits {
    /// Conservative defaults for a trainer whose ranges could not be read.
    pub const FALLBACK: Self = Self {
        power_w: (0, 1000),
        resistance: (0, 100),
    };

    /// Parse the raw 2AD8 and 2AD6 values (sint16 minimum, sint16 maximum, uint16 increment).
    pub fn from_ranges(power: &[u8], resistance: &[u8]) -> Option<Self> {
        let pair = |b: &[u8]| -> Option<(i16, i16)> {
            let min = i16::from_le_bytes([*b.first()?, *b.get(1)?]);
            let max = i16::from_le_bytes([*b.get(2)?, *b.get(3)?]);
            (min <= max).then_some((min, max))
        };
        Some(Self {
            power_w: pair(power)?,
            resistance: pair(resistance)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Write these bytes to the trainer (possibly clamped).
    Forward(Vec<u8>),
    /// Do not write; send this FTMS response back to the app.
    Reject([u8; 3]),
}

fn reject(op: u8, result: u8) -> Verdict {
    Verdict::Reject([RESPONSE, op, result])
}

pub fn filter(command: &[u8], limits: &Limits) -> Verdict {
    let Some((&op, params)) = command.split_first() else {
        return reject(0, RESULT_INVALID_PARAMETER);
    };
    let clamp16 = |v: i16, (lo, hi): (i16, i16)| v.clamp(lo, hi);
    match (op, params.len()) {
        (REQUEST_CONTROL | RESET | START_OR_RESUME, 0) => Verdict::Forward(vec![op]),
        (STOP_OR_PAUSE, 1) if matches!(params[0], 1 | 2) => Verdict::Forward(command.to_vec()),
        (SET_TARGET_POWER, 2) => {
            let w = clamp16(i16::from_le_bytes([params[0], params[1]]), limits.power_w);
            let [a, b] = w.to_le_bytes();
            Verdict::Forward(vec![op, a, b])
        }
        (SET_TARGET_RESISTANCE, 1) => {
            let (lo, hi) = limits.resistance;
            let lo = u8::try_from(lo.max(0)).unwrap_or(0);
            let hi = u8::try_from(hi.max(0)).unwrap_or(u8::MAX);
            Verdict::Forward(vec![op, params[0].clamp(lo, hi.max(lo))])
        }
        (SET_SIMULATION, 6) => {
            let grade = i16::from_le_bytes([params[2], params[3]]).clamp(-MAX_GRADE, MAX_GRADE);
            let mut out = command.to_vec();
            out[3..5].copy_from_slice(&grade.to_le_bytes());
            Verdict::Forward(out)
        }
        (REQUEST_CONTROL | RESET | START_OR_RESUME | STOP_OR_PAUSE | SET_TARGET_POWER, _)
        | (SET_TARGET_RESISTANCE | SET_SIMULATION, _) => reject(op, RESULT_INVALID_PARAMETER),
        _ => reject(op, RESULT_NOT_SUPPORTED),
    }
}

/// Bounded write queue. A newer target or simulation command replaces a pending one of the
/// same kind, so a slow trainer always receives the latest value rather than a backlog.
#[derive(Debug, Default)]
pub struct CommandQueue {
    pending: VecDeque<Vec<u8>>,
}

impl CommandQueue {
    pub const CAPACITY: usize = 8;

    /// Returns false when the queue is full and the command was dropped.
    pub fn push(&mut self, command: Vec<u8>) -> bool {
        let op = command.first().copied();
        let replaceable = matches!(
            op,
            Some(SET_TARGET_POWER | SET_TARGET_RESISTANCE | SET_SIMULATION)
        );
        if replaceable
            && let Some(slot) = self.pending.iter_mut().find(|c| c.first().copied() == op)
        {
            *slot = command;
            return true;
        }
        if self.pending.len() >= Self::CAPACITY {
            return false;
        }
        self.pending.push_back(command);
        true
    }

    pub fn pop(&mut self) -> Option<Vec<u8>> {
        self.pending.pop_front()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Captured X2 Max ranges: power 0-2000 W, resistance 0-20.0.
    fn x2() -> Limits {
        match Limits::from_ranges(&[0, 0, 0xd0, 0x07, 1, 0], &[0, 0, 0xc8, 0, 1, 0]) {
            Some(l) => l,
            None => panic!("captured ranges rejected"),
        }
    }

    #[test]
    fn captured_ranges_parse() {
        assert_eq!(
            x2(),
            Limits {
                power_w: (0, 2000),
                resistance: (0, 200)
            }
        );
        assert_eq!(Limits::from_ranges(&[1], &[]), None);
    }

    #[test]
    fn allowed_commands_pass_and_targets_clamp() {
        let l = x2();
        assert_eq!(filter(&[0x00], &l), Verdict::Forward(vec![0x00]));
        assert_eq!(filter(&[0x07], &l), Verdict::Forward(vec![0x07]));
        assert_eq!(
            filter(&[0x08, 0x01], &l),
            Verdict::Forward(vec![0x08, 0x01])
        );
        assert_eq!(
            filter(&[0x05, 0xc8, 0x00], &l),
            Verdict::Forward(vec![0x05, 0xc8, 0x00])
        );
        // 3000 W is clamped to the advertised 2000 W; negative power to 0.
        assert_eq!(
            filter(&[0x05, 0xb8, 0x0b], &l),
            Verdict::Forward(vec![0x05, 0xd0, 0x07])
        );
        assert_eq!(
            filter(&[0x05, 0xff, 0xff], &l),
            Verdict::Forward(vec![0x05, 0, 0])
        );
        assert_eq!(filter(&[0x04, 250], &l), Verdict::Forward(vec![0x04, 200]));
    }

    #[test]
    fn simulation_grade_is_limited_other_fields_kept() {
        // wind 0, grade +50 %, crr 0x28, cw 0x33
        let cmd = [0x11, 0, 0, 0x88, 0x13, 0x28, 0x33];
        let Verdict::Forward(out) = filter(&cmd, &x2()) else {
            panic!("rejected")
        };
        assert_eq!(i16::from_le_bytes([out[3], out[4]]), 3000);
        assert_eq!((out[5], out[6]), (0x28, 0x33));
        let ok = [0x11, 0, 0, 0xf4, 0x01, 0x28, 0x33];
        assert_eq!(filter(&ok, &x2()), Verdict::Forward(ok.to_vec()));
    }

    #[test]
    fn unknown_or_malformed_commands_never_forward() {
        let l = x2();
        assert_eq!(
            filter(&[0x13, 1], &l),
            Verdict::Reject([0x80, 0x13, RESULT_NOT_SUPPORTED])
        );
        assert_eq!(
            filter(&[0x05, 1], &l),
            Verdict::Reject([0x80, 0x05, RESULT_INVALID_PARAMETER])
        );
        assert_eq!(
            filter(&[0x08, 9], &l),
            Verdict::Reject([0x80, 0x08, RESULT_INVALID_PARAMETER])
        );
        assert_eq!(
            filter(&[], &l),
            Verdict::Reject([0x80, 0, RESULT_INVALID_PARAMETER])
        );
        for op in 0..=u8::MAX {
            for len in 0..9 {
                let cmd: Vec<u8> = std::iter::once(op)
                    .chain(std::iter::repeat_n(0xff, len))
                    .collect();
                if let Verdict::Forward(out) = filter(&cmd, &l) {
                    assert!(
                        matches!(op, 0x00 | 0x01 | 0x04 | 0x05 | 0x07 | 0x08 | 0x11),
                        "{op:#x}"
                    );
                    assert_eq!(out.len(), cmd.len());
                }
            }
        }
    }

    #[test]
    fn queue_coalesces_targets_and_is_bounded() {
        let mut q = CommandQueue::default();
        assert!(q.push(vec![0x00]));
        assert!(q.push(vec![0x11, 0, 0, 1, 0, 0, 0]));
        assert!(q.push(vec![0x11, 0, 0, 2, 0, 0, 0]));
        assert_eq!(q.pop(), Some(vec![0x00]));
        assert_eq!(q.pop(), Some(vec![0x11, 0, 0, 2, 0, 0, 0]));
        assert!(q.is_empty());
        for _ in 0..CommandQueue::CAPACITY {
            assert!(q.push(vec![0x07]));
        }
        assert!(!q.push(vec![0x07]));
    }
}
