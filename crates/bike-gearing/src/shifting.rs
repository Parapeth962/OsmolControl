//! Virtual shifting: turn the app's road simulation and the rider's cadence into a trainer power
//! target for the selected gear. Pure; the caller supplies time steps.
use crate::{Drivetrain, Gear};

const G: f64 = 9.80665;
/// Largest change of the power target per second, so cadence spikes ramp smoothly. Shifts are
/// not limited: the new gear's power applies at once.
const MAX_SLEW_W_PER_S: f64 = 250.0;

/// FTMS "Set Indoor Bike Simulation Parameters" (op 0x11), in SI units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Simulation {
    pub wind_mps: f64,
    pub grade_percent: f64,
    pub crr: f64,
    /// Wind resistance coefficient in kg/m (= CdA·ρ, as in FTMS/FE-C; MyWhoosh sends 0.51).
    pub cw: f64,
}

impl Simulation {
    /// Parse the full control-point command, including the op code.
    pub fn from_ftms(command: &[u8]) -> Option<Self> {
        let [0x11, w0, w1, g0, g1, crr, cw] = *command else {
            return None;
        };
        Some(Self {
            wind_mps: f64::from(i16::from_le_bytes([w0, w1])) * 0.001,
            grade_percent: f64::from(i16::from_le_bytes([g0, g1])) * 0.01,
            crr: f64::from(crr) * 0.0001,
            cw: f64::from(cw) * 0.01,
        })
    }

    /// Power needed to hold `speed_mps` on this road with total mass `mass_kg`. Never negative:
    /// descending is free-wheeling, the trainer cannot push the rider.
    pub fn power_w(&self, speed_mps: f64, mass_kg: f64) -> f64 {
        let theta = (self.grade_percent / 100.0).atan();
        let gravity = mass_kg * G * theta.sin();
        let rolling = mass_kg * G * self.crr * theta.cos();
        let air = speed_mps + self.wind_mps;
        let aero = 0.5 * self.cw * air * air.abs();
        ((gravity + rolling + aero) * speed_mps).max(0.0)
    }
}

#[derive(Debug, Clone)]
pub struct VirtualShifter {
    drivetrain: Drivetrain,
    gears: Vec<Gear>,
    /// 0-based index into `gears`.
    index: usize,
    mass_kg: f64,
    /// Multiplier for the app's grade; apps may send a fraction of the road grade (trainer
    /// difficulty), 2.0 restores the full grade from a 50 % setting.
    grade_scale: f64,
    simulation: Option<Simulation>,
    target_w: Option<f64>,
}

impl VirtualShifter {
    pub fn new(drivetrain: Drivetrain, mass_kg: f64) -> Self {
        let gears = drivetrain.gears();
        let index = drivetrain.default_gear().saturating_sub(1);
        Self {
            drivetrain,
            gears,
            index,
            mass_kg,
            grade_scale: 1.0,
            simulation: None,
            target_w: None,
        }
    }

    pub fn with_grade_scale(mut self, grade_scale: f64) -> Self {
        self.grade_scale = grade_scale;
        self
    }

    /// The grade the physics uses, i.e. after scaling.
    pub fn grade_percent(&self) -> Option<f64> {
        self.simulation.map(|s| s.grade_percent)
    }

    pub fn gear(&self) -> Gear {
        self.gears[self.index]
    }

    pub fn gear_count(&self) -> usize {
        self.gears.len()
    }

    /// `harder` moves to a higher ratio. Returns false at either end of the range.
    pub fn shift(&mut self, harder: bool) -> bool {
        let next = if harder {
            (self.index + 1).min(self.gears.len() - 1)
        } else {
            self.index.saturating_sub(1)
        };
        let moved = next != self.index;
        self.index = next;
        if moved {
            // Skip the slew limit so the shift is felt immediately.
            self.target_w = None;
        }
        moved
    }

    pub fn set_simulation(&mut self, simulation: Simulation) {
        self.simulation = Some(Simulation {
            grade_percent: simulation.grade_percent * self.grade_scale,
            ..simulation
        });
    }

    /// Forget the road, e.g. after the app switched to ERG or reset the trainer.
    pub fn clear_simulation(&mut self) {
        self.simulation = None;
        self.target_w = None;
    }

    pub fn virtual_speed_kmh(&self, cadence_rpm: f64) -> f64 {
        self.drivetrain.speed_kmh(&self.gear(), cadence_rpm)
    }

    /// Next power target after `dt_s` seconds, or `None` without a road simulation.
    /// Unknown cadence (`None`) or zero cadence aims for 0 W, i.e. the trainer's minimum.
    pub fn update(&mut self, cadence_rpm: Option<f64>, dt_s: f64) -> Option<f64> {
        let simulation = self.simulation?;
        let speed = self.virtual_speed_kmh(cadence_rpm.unwrap_or(0.0).max(0.0)) / 3.6;
        let wanted = simulation.power_w(speed, self.mass_kg);
        let step = MAX_SLEW_W_PER_S * dt_s.max(0.0);
        let next = match self.target_w {
            Some(previous) => previous + (wanted - previous).clamp(-step, step),
            None => wanted,
        };
        self.target_w = Some(next);
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat() -> Simulation {
        Simulation {
            wind_mps: 0.0,
            grade_percent: 0.0,
            crr: 0.004,
            cw: 0.51,
        }
    }
    fn shifter() -> VirtualShifter {
        let d = Drivetrain::new(
            &[50, 34],
            &[11, 12, 13, 14, 15, 17, 19, 21, 23, 25, 28],
            2136,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        VirtualShifter::new(d, 84.0)
    }

    #[test]
    fn parses_captured_mywhoosh_command() {
        // From the live bridge log: grade 1.79 %, Crr 0x35, Cw 0x33.
        let s = Simulation::from_ftms(&[0x11, 0, 0, 0xb3, 0, 0x35, 0x33])
            .unwrap_or_else(|| panic!("parse"));
        assert!((s.grade_percent - 1.79).abs() < 1e-9);
        assert!((s.crr - 0.0053).abs() < 1e-12);
        assert!((s.cw - 0.51).abs() < 1e-12);
        assert_eq!(Simulation::from_ftms(&[0x11, 0]), None);
    }

    #[test]
    fn physics_is_plausible() {
        // 84 kg at 30 km/h on the flat with Crr 0.004 and CdA·ρ 0.51: about 175 W.
        let p = flat().power_w(30.0 / 3.6, 84.0);
        assert!((165.0..185.0).contains(&p), "{p}");
        let climb = Simulation {
            grade_percent: 6.0,
            ..flat()
        };
        assert!(climb.power_w(3.0, 84.0) > 150.0);
        let descent = Simulation {
            grade_percent: -8.0,
            ..flat()
        };
        assert_eq!(descent.power_w(5.0, 84.0), 0.0);
        let headwind = Simulation {
            wind_mps: 3.0,
            ..flat()
        };
        assert!(headwind.power_w(8.0, 84.0) > flat().power_w(8.0, 84.0));
    }

    #[test]
    fn harder_gear_needs_more_power_at_same_cadence() {
        let mut s = shifter();
        s.set_simulation(Simulation {
            grade_percent: 3.0,
            ..flat()
        });
        let low = s.update(Some(85.0), 100.0).unwrap_or_default();
        assert!(s.shift(true));
        assert!(s.shift(true));
        let high = s.update(Some(85.0), 100.0).unwrap_or_default();
        assert!(high > low, "{high} <= {low}");
    }

    #[test]
    fn grade_scale_multiplies_app_grade() {
        let mut s = shifter().with_grade_scale(2.0);
        s.set_simulation(Simulation {
            grade_percent: 2.5,
            ..flat()
        });
        assert_eq!(s.grade_percent(), Some(5.0));
        let scaled = s.update(Some(85.0), 100.0).unwrap_or_default();
        let mut plain = shifter();
        plain.set_simulation(Simulation {
            grade_percent: 5.0,
            ..flat()
        });
        assert_eq!(plain.update(Some(85.0), 100.0), Some(scaled));
    }

    #[test]
    fn no_target_without_road_and_slew_is_limited() {
        let mut s = shifter();
        assert_eq!(s.update(Some(90.0), 1.0), None);
        s.set_simulation(flat());
        let first = s.update(Some(60.0), 0.5).unwrap_or_default();
        let jump = s.update(Some(120.0), 0.5).unwrap_or_default();
        assert!(jump - first <= MAX_SLEW_W_PER_S * 0.5 + 1e-9);
        // A shift jumps straight to the new gear's power.
        assert!(s.shift(true));
        let shifted = s.update(Some(120.0), 0.01).unwrap_or_default();
        let wanted = s
            .simulation
            .map(|road| road.power_w(s.virtual_speed_kmh(120.0) / 3.6, 84.0));
        assert_eq!(Some(shifted), wanted);
        let stopped = s.update(None, 100.0).unwrap_or_default();
        assert_eq!(stopped, 0.0);
        s.clear_simulation();
        assert_eq!(s.update(Some(90.0), 1.0), None);
    }

    #[test]
    fn shifting_stops_at_the_ends() {
        let mut s = shifter();
        while s.shift(false) {}
        assert_eq!(s.gear().number, 1);
        assert!(!s.shift(false));
        while s.shift(true) {}
        assert_eq!(s.gear().number, s.gear_count());
        assert_eq!((s.gear().chainring, s.gear().cog), (50, 11));
    }
}
