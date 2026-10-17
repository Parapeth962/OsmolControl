//! Virtual drivetrain: gear lists, calculator figures and setup templates. Pure, no I/O.
//!
//! Every chainring × cog combination is a gear (2×7 gives 14 gears, 2×10 gives 20), ordered from
//! easiest to hardest. Front-derailleur behaviour is not simulated.
use serde::Serialize;

mod shifting;
mod templates;
pub use shifting::{Simulation, VirtualShifter};
pub use templates::{Brand, Filter, TEMPLATES, Template, template};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GearError {
    #[error("1 to 3 chainrings required, got {0}")]
    Chainrings(usize),
    #[error("1 to 60 cogs required, got {0}")]
    Cogs(usize),
    #[error("tooth count {0} outside 9..=160")]
    Teeth(u16),
    #[error("wheel circumference {0} mm outside 1500..=2500")]
    Wheel(u16),
    #[error("tooth count {0} appears twice on the same cassette or crank")]
    Duplicate(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Category {
    Road,
    Gravel,
    /// Single-speed ratios and very wide virtual ladders (30 to 50 gears).
    Special,
}

impl Category {
    pub const ALL: [Self; 3] = [Self::Road, Self::Gravel, Self::Special];

    pub fn label(self) -> &'static str {
        match self {
            Self::Road => "Road",
            Self::Gravel => "Gravel",
            Self::Special => "Single speed & wide",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Drivetrain {
    pub chainrings: Vec<u16>,
    pub cassette: Vec<u16>,
    pub wheel_circumference_mm: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Gear {
    /// 1 = easiest.
    pub number: usize,
    pub chainring: u16,
    pub cog: u16,
    pub ratio: f64,
}

impl Drivetrain {
    pub fn new(
        chainrings: &[u16],
        cassette: &[u16],
        wheel_circumference_mm: u16,
    ) -> Result<Self, GearError> {
        if !(1..=3).contains(&chainrings.len()) {
            return Err(GearError::Chainrings(chainrings.len()));
        }
        // Virtual drivetrains go beyond hardware: single speeds and ladders of up to 60 cogs.
        if !(1..=60).contains(&cassette.len()) {
            return Err(GearError::Cogs(cassette.len()));
        }
        if let Some(&t) = chainrings
            .iter()
            .chain(cassette)
            .find(|t| !(9..=160).contains(*t))
        {
            return Err(GearError::Teeth(t));
        }
        if !(1500..=2500).contains(&wheel_circumference_mm) {
            return Err(GearError::Wheel(wheel_circumference_mm));
        }
        for teeth in [chainrings, cassette] {
            if let Some(w) = teeth
                .iter()
                .enumerate()
                .find(|(i, t)| teeth[..*i].contains(t))
            {
                return Err(GearError::Duplicate(*w.1));
            }
        }
        Ok(Self {
            chainrings: chainrings.to_vec(),
            cassette: cassette.to_vec(),
            wheel_circumference_mm,
        })
    }

    /// All combinations, easiest first. Equal ratios keep big-ring-last order for stability.
    pub fn gears(&self) -> Vec<Gear> {
        let mut gears: Vec<Gear> = self
            .chainrings
            .iter()
            .flat_map(|&chainring| {
                self.cassette.iter().map(move |&cog| Gear {
                    number: 0,
                    chainring,
                    cog,
                    ratio: f64::from(chainring) / f64::from(cog),
                })
            })
            .collect();
        gears.sort_by(|a, b| {
            a.ratio
                .total_cmp(&b.ratio)
                .then(a.chainring.cmp(&b.chainring))
        });
        for (i, g) in gears.iter_mut().enumerate() {
            g.number = i + 1;
        }
        gears
    }

    /// Starting gear: the middle of the range, so both directions are available at once.
    pub fn default_gear(&self) -> usize {
        (self.chainrings.len() * self.cassette.len() / 2).max(1)
    }

    /// Distance travelled per crank revolution, in metres.
    pub fn development_m(&self, gear: &Gear) -> f64 {
        gear.ratio * f64::from(self.wheel_circumference_mm) / 1000.0
    }

    pub fn speed_kmh(&self, gear: &Gear, cadence_rpm: f64) -> f64 {
        self.development_m(gear) * cadence_rpm * 60.0 / 1000.0
    }
}

/// "1 gear", "22 gears".
pub fn gear_count(n: usize) -> String {
    if n == 1 {
        "1 gear".into()
    } else {
        format!("{n} gears")
    }
}

/// Percentage change in ratio from `from` to `to`.
pub fn step_percent(from: &Gear, to: &Gear) -> f64 {
    (to.ratio / from.ratio - 1.0) * 100.0
}

/// Parse "11-12-13" or "50,34" style tooth lists.
pub fn parse_teeth(text: &str) -> Option<Vec<u16>> {
    text.split([',', '-', '/', ' '])
        .filter(|s| !s.is_empty())
        .map(|s| s.trim().parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_by_seven_is_fourteen_sorted_gears() {
        let d = Drivetrain::new(&[50, 34], &[14, 16, 18, 20, 22, 24, 28], 2105)
            .unwrap_or_else(|e| panic!("{e}"));
        let g = d.gears();
        assert_eq!(g.len(), 14);
        assert!(g.windows(2).all(|w| w[0].ratio <= w[1].ratio));
        assert_eq!((g[0].chainring, g[0].cog), (34, 28));
        assert_eq!((g[13].chainring, g[13].cog), (50, 14));
        assert_eq!(g[0].number, 1);
        assert_eq!(d.default_gear(), 7);
    }

    #[test]
    fn calculator_figures() {
        let d =
            Drivetrain::new(&[50], &[10, 11, 12, 13, 14], 2100).unwrap_or_else(|e| panic!("{e}"));
        let top = d.gears()[4];
        assert_eq!(top.ratio, 5.0);
        assert!((d.development_m(&top) - 10.5).abs() < 1e-9);
        assert!((d.speed_kmh(&top, 90.0) - 56.7).abs() < 1e-9);
        let g = d.gears();
        assert!((step_percent(&g[0], &g[1]) - (14.0 / 13.0 - 1.0) * 100.0).abs() < 1e-9);
    }

    #[test]
    fn validation() {
        assert_eq!(
            Drivetrain::new(&[], &[11, 13], 2100),
            Err(GearError::Chainrings(0))
        );
        assert_eq!(Drivetrain::new(&[50], &[], 2100), Err(GearError::Cogs(0)));
        let too_many: Vec<u16> = (10..71).collect();
        assert_eq!(
            Drivetrain::new(&[50], &too_many, 2100),
            Err(GearError::Cogs(61))
        );
        assert_eq!(
            Drivetrain::new(&[161], &[11, 13], 2100),
            Err(GearError::Teeth(161))
        );
        assert_eq!(
            Drivetrain::new(&[50], &[11, 13], 900),
            Err(GearError::Wheel(900))
        );
        assert_eq!(
            Drivetrain::new(&[50], &[11, 13, 11], 2100),
            Err(GearError::Duplicate(11))
        );
        assert_eq!(
            Drivetrain::new(&[50, 50], &[11, 13], 2100),
            Err(GearError::Duplicate(50))
        );
        // A single speed is one gear, and the ride starts in it.
        let single = Drivetrain::new(&[46], &[16], 2100);
        assert_eq!(single.as_ref().map(|d| d.gears().len()), Ok(1));
        assert_eq!(single.map(|d| d.default_gear()), Ok(1));
    }

    #[test]
    fn parse_lists() {
        assert_eq!(parse_teeth("50/34"), Some(vec![50, 34]));
        assert_eq!(parse_teeth("11-12-13"), Some(vec![11, 12, 13]));
        assert_eq!(parse_teeth("11,x"), None);
    }
}
