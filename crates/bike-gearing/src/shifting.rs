//! Virtual shifting: turn the app's road simulation and the rider's cadence into a trainer power
//! target for the selected gear. Pure; the caller supplies time steps.
use crate::{Drivetrain, Gear};

const G: f64 = 9.80665;
/// Largest change of the power target per second, so cadence spikes ramp smoothly. Shifts are
/// not limited: the new gear's power applies at once.
const MAX_SLEW_W_PER_S: f64 = 250.0;
/// Largest wind resistance coefficient an FTMS simulation command can carry (uint8, 0.01 kg/m).
const MAX_CW: f64 = 2.55;
/// Steepest grade sent to a trainer; the control-point filter allows ±30 % as well.
const MAX_GRADE_PERCENT: f64 = 30.0;

/// Estimates the gear the rider's chain really sits in on the trainer (chainring/cog) from the
/// trainer's wheel speed and the cadence, as `speed / (cadence · wheel circumference)`.
///
/// Only steady pedalling counts: a full window of readings that agree closely. While the cadence
/// changes the flywheel lags or freewheels, which would skew the ratio. Once known, the estimate
/// moves only for a clear change (the rider shifted on the bike), so it never drifts the load.
#[derive(Debug, Clone)]
pub struct RealChain {
    wheel_m: f64,
    samples: std::collections::VecDeque<f64>,
    ratio: Option<f64>,
}

impl RealChain {
    /// Readings in the window: 6 s of the usual 4 Hz trainer data.
    const WINDOW: usize = 24;
    const MIN_CADENCE_RPM: f64 = 40.0;
    const MIN_SPEED_KMH: f64 = 5.0;
    /// Largest spread (interquartile range over the median) that still counts as steady.
    const MAX_SPREAD: f64 = 0.04;
    /// Smallest relative change taken as a new real gear.
    const MIN_CHANGE: f64 = 0.08;

    pub fn new(wheel_circumference_mm: u16) -> Self {
        Self {
            wheel_m: f64::from(wheel_circumference_mm) / 1000.0,
            samples: std::collections::VecDeque::with_capacity(Self::WINDOW),
            ratio: None,
        }
    }

    /// Starts from a ratio learned earlier (the last ride). It is checked against the trainer
    /// like any estimate and replaced once steady pedalling shows a clearly different one.
    pub fn with_ratio(mut self, ratio: Option<f64>) -> Self {
        self.ratio = ratio.filter(|r| r.is_finite() && (0.3..=8.0).contains(r));
        self
    }

    pub fn ratio(&self) -> Option<f64> {
        self.ratio
    }

    /// Feed one trainer reading. Returns the new ratio when the estimate changed.
    pub fn observe(&mut self, speed_kmh: f64, cadence_rpm: f64) -> Option<f64> {
        if cadence_rpm < Self::MIN_CADENCE_RPM || speed_kmh < Self::MIN_SPEED_KMH {
            // Coasting or stopping breaks the steady run.
            self.samples.clear();
            return None;
        }
        if self.samples.len() == Self::WINDOW {
            self.samples.pop_front();
        }
        self.samples
            .push_back(speed_kmh / 3.6 / (cadence_rpm / 60.0 * self.wheel_m));
        if self.samples.len() < Self::WINDOW {
            return None;
        }
        let mut sorted: Vec<f64> = self.samples.iter().copied().collect();
        sorted.sort_by(f64::total_cmp);
        let at = |q: usize| sorted[(sorted.len() - 1) * q / 4];
        let median = at(2);
        if (at(3) - at(1)) / median > Self::MAX_SPREAD {
            return None;
        }
        let changed = self
            .ratio
            .is_none_or(|r| (median / r - 1.0).abs() > Self::MIN_CHANGE);
        changed.then(|| {
            self.ratio = Some(median);
            median
        })
    }
}

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

    /// The full control-point command (op code 0x11), rounded and clamped to the FTMS fields:
    /// wind sint16 in 0.001 m/s, grade sint16 in 0.01 %, Crr uint8 in 0.0001, Cw uint8 in 0.01 kg/m.
    pub fn to_ftms(&self) -> Vec<u8> {
        let i16_field = |v: f64| {
            // Clamped to the field's range first, so the cast cannot wrap.
            (v.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16).to_le_bytes()
        };
        let u8_field = |v: f64| v.round().clamp(0.0, 255.0) as u8;
        let [w0, w1] = i16_field(self.wind_mps / 0.001);
        let [g0, g1] = i16_field(self.grade_percent / 0.01);
        vec![
            0x11,
            w0,
            w1,
            g0,
            g1,
            u8_field(self.crr / 0.0001),
            u8_field(self.cw / 0.01),
        ]
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

    pub fn mass_kg(&self) -> f64 {
        self.mass_kg
    }

    pub fn drivetrain(&self) -> &Drivetrain {
        &self.drivetrain
    }

    /// The road to send a trainer that simulates it itself, so the virtual gear is felt the way
    /// a real one is: the trainer answers every change of its flywheel speed at once, with no
    /// cadence reading in the loop.
    ///
    /// The rider's chain sits on `real_ratio` (chainring/cog) on the trainer; the selected gear
    /// is `k` times that. At the same cadence the virtual bike goes `k` times faster than the
    /// trainer's wheel, so the trainer must ask, at its own speed `v`, for the power the road
    /// asks at `k·v`: a wheel force of `k·F_road(k·v)`. Gravity and rolling scale by `k` (sent as
    /// grade), air drag by `k³` (sent as Cw, wind as `wind/k`). Air drag beyond the largest Cw
    /// FTMS can carry is added to the grade at `trainer_speed_mps`. The trainer is assumed to use
    /// the rider's mass; any difference shows up as a difficulty offset, not as a change of feel.
    pub fn trainer_road(&self, real_ratio: f64, trainer_speed_mps: f64) -> Option<Simulation> {
        let road = self.simulation?;
        if !(real_ratio.is_finite() && real_ratio > 0.0) {
            return None;
        }
        let k = self.gear().ratio / real_ratio;
        let cw = (road.cw * k.powi(3)).min(MAX_CW);
        let wind = road.wind_mps / k;
        let drag = |cw: f64, air: f64| 0.5 * cw * air * air.abs();
        let v = trainer_speed_mps.max(0.0);
        let missing_air = k * drag(road.cw, k * v + road.wind_mps) - drag(cw, v + wind);
        let theta = (road.grade_percent / 100.0).atan();
        // sin θ' + Crr·cos θ' = x, solved for the trainer's slope θ'.
        let x = k * (theta.sin() + road.crr * theta.cos()) + missing_air / (self.mass_kg * G);
        let norm = (1.0 + road.crr * road.crr).sqrt();
        let slope = (x / norm).clamp(-1.0, 1.0).asin() - road.crr.atan();
        Some(Simulation {
            wind_mps: wind,
            grade_percent: (slope.tan() * 100.0).clamp(-MAX_GRADE_PERCENT, MAX_GRADE_PERCENT),
            crr: road.crr,
            cw,
        })
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
    fn simulation_command_round_trips() {
        let captured = [0x11, 0, 0, 0xb3, 0, 0x35, 0x33];
        let s = Simulation::from_ftms(&captured).unwrap_or_else(|| panic!("parse"));
        assert_eq!(s.to_ftms(), captured);
        let wild = Simulation {
            wind_mps: -1.5,
            grade_percent: 900.0,
            crr: 1.0,
            cw: 9.0,
        };
        let c = wild.to_ftms();
        assert_eq!(i16::from_le_bytes([c[1], c[2]]), -1500);
        assert_eq!(i16::from_le_bytes([c[3], c[4]]), 32767);
        assert_eq!((c[5], c[6]), (255, 255));
    }

    /// The trainer, simulating the sent road at its own wheel speed, asks for the power the
    /// real road asks at the virtual speed: the same cadence costs the same as outside.
    #[test]
    fn trainer_road_costs_what_the_virtual_gear_costs() {
        let mut s = shifter();
        let real = 34.0 / 17.0;
        for (grade, wind) in [(0.0, 0.0), (4.0, 0.0), (-2.0, 0.0), (1.0, 2.0)] {
            s.set_simulation(Simulation {
                grade_percent: grade,
                wind_mps: wind,
                ..flat()
            });
            while s.shift(false) {}
            loop {
                let k = s.gear().ratio / real;
                for v in [3.0, 6.0, 9.0] {
                    let road = s.simulation.map(|r| r.power_w(k * v, 84.0));
                    let trainer = s.trainer_road(real, v).map(|t| t.power_w(v, 84.0));
                    let (Some(road), Some(trainer)) = (road, trainer) else {
                        panic!("no road");
                    };
                    if s.trainer_road(real, v)
                        .is_some_and(|t| t.grade_percent.abs() < MAX_GRADE_PERCENT)
                    {
                        assert!(
                            (road - trainer).abs() < 0.5 + road * 1e-3,
                            "grade {grade}, gear {}, {v} m/s: road {road} W, trainer {trainer} W",
                            s.gear().number
                        );
                    }
                }
                if !s.shift(true) {
                    break;
                }
            }
        }
    }

    #[test]
    fn same_gear_as_the_real_chain_sends_the_road_unchanged() {
        let mut s = shifter();
        s.set_simulation(Simulation {
            grade_percent: 5.0,
            ..flat()
        });
        let real = s.gear().ratio;
        let sent = s
            .trainer_road(real, 5.0)
            .unwrap_or_else(|| panic!("no road"));
        assert!((sent.grade_percent - 5.0).abs() < 1e-9, "{sent:?}");
        assert!((sent.cw - flat().cw).abs() < 1e-12);
        // A harder gear is a steeper road for the trainer; an easier one is flatter.
        assert!(s.shift(true));
        let harder = s.trainer_road(real, 5.0).map(|t| t.grade_percent);
        assert!(harder.is_some_and(|g| g > 5.0));
        assert!(s.shift(false) && s.shift(false));
        let easier = s.trainer_road(real, 5.0).map(|t| t.grade_percent);
        assert!(easier.is_some_and(|g| g < 5.0));
        assert_eq!(s.trainer_road(0.0, 5.0), None);
        s.clear_simulation();
        assert_eq!(s.trainer_road(real, 5.0), None);
    }

    #[test]
    fn real_chain_is_learned_from_steady_pedalling_only() {
        let mut chain = RealChain::new(2136);
        // 62 rpm at 19.47 km/h (captured X2 Max frame): about 2.45, e.g. 34x14.
        let mut found = None;
        for _ in 0..RealChain::WINDOW {
            found = found.or(chain.observe(19.47, 62.0));
        }
        let ratio = found.unwrap_or_else(|| panic!("not learned"));
        assert!((ratio - 2.448).abs() < 0.01, "{ratio}");
        // Noise and a cadence change do not move it.
        for i in 0..100 {
            let wobble = if i % 2 == 0 { 0.5 } else { -0.5 };
            assert_eq!(chain.observe(19.47, 62.0 + wobble), None);
        }
        assert_eq!(chain.observe(19.0, 30.0), None, "too slow to count");
        // Unsteady readings (flywheel lagging a cadence surge) are not used.
        let mut fresh = RealChain::new(2136);
        for i in 0..RealChain::WINDOW * 2 {
            let surge = 50.0 + (i % 6) as f64 * 8.0;
            assert_eq!(fresh.observe(19.47, surge), None);
        }
        // A real shift on the bike: a new steady ratio replaces the old one.
        let mut moved = None;
        for _ in 0..RealChain::WINDOW {
            moved = moved.or(chain.observe(24.0, 62.0));
        }
        assert!(moved.is_some_and(|r| r > ratio * 1.2));
        assert_eq!(chain.ratio(), moved);
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
