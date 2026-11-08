//! Rides through the router against a simulated trainer and rider, in simulated time.
//!
//! The trainer follows FTMS: in simulation mode it computes the resistance from its own wheel
//! speed and the road it was sent; in ERG mode it holds the target power. The rider pushes a
//! set power; the flywheel's inertia turns any difference into acceleration. Telemetry is sent
//! at 4 Hz, quantized like a ThinkRider X2 Max (whole rpm, 0.01 km/h) with sensor noise, and
//! control writes arrive after the trainer link's pacing delay.
//!
//! `cargo test -p bike-session --test ride -- --nocapture` prints what each ride measured.
use bike_dircon::sig;
use bike_gearing::{Drivetrain, Simulation, VirtualShifter};
use bike_protocol::Action;
use bike_protocol::ftms_control::Limits;
use bike_session::ShiftFeel;
use bike_session::bridge::Router;
use std::time::{Duration, Instant};

const INDOOR_BIKE_DATA: u16 = 0x2ad2;
const MASS_KG: f64 = 84.0;
/// Rider's real chain on the trainer: 50x17 on a bike with 2105 mm tyres.
const REAL_RATIO: f64 = 50.0 / 17.0;
/// The trainer computes its speed with its own wheel size, not the bike's.
const TRAINER_WHEEL_M: f64 = 2.096;
/// Flywheel inertia felt at the wheel, as an equivalent mass (a mid-range direct-drive trainer).
const FLYWHEEL_KG: f64 = 20.0;
/// Control write delay: the trainer link paces writes at 250 ms, plus Bluetooth.
const WRITE_DELAY: Duration = Duration::from_millis(300);
const STEP: Duration = Duration::from_millis(10);

/// MyWhoosh's simulation command for `grade` percent (wind 0, Crr 0.0053, Cw 0.51), at the half
/// grade it really sends.
fn road(grade: f64) -> Vec<u8> {
    Simulation {
        wind_mps: 0.0,
        grade_percent: grade * bike_session::MYWHOOSH_GRADE_SHARE,
        crr: 0.0053,
        cw: 0.51,
    }
    .to_ftms()
}

/// Deterministic noise in -1..1.
struct Noise(u64);
impl Noise {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

enum Trainer {
    Free,
    Erg(f64),
    Road(Simulation),
}

struct Ride {
    router: Router,
    trainer: Trainer,
    /// Trainer wheel speed, m/s.
    v: f64,
    rider_w: f64,
    t0: Instant,
    t: Duration,
    next_telemetry: Duration,
    next_tick: Duration,
    in_flight: Vec<(Duration, Vec<u8>)>,
    noise: Noise,
    /// (time, opcode) of every write that reached the trainer.
    written: Vec<(Duration, u8)>,
    wheel_m: f64,
    /// The rider's real chain on the trainer (chainring/cog).
    real: f64,
}

impl Ride {
    fn new(feel: ShiftFeel) -> Self {
        let drivetrain =
            Drivetrain::new(&[50, 34], &[11, 12, 13, 14, 15, 17, 19, 21, 24, 28], 2105)
                .unwrap_or_else(|e| panic!("{e}"));
        let shifter = VirtualShifter::new(drivetrain, MASS_KG).with_grade_scale(2.0);
        let mut router = Router::new(Limits::FALLBACK, Some(shifter)).with_feel(feel);
        router.app_write(&[0x00]);
        router.app_write(&road(0.0));
        Self {
            router,
            trainer: Trainer::Free,
            v: 8.0,
            rider_w: 200.0,
            t0: Instant::now(),
            t: Duration::ZERO,
            next_telemetry: Duration::ZERO,
            next_tick: Duration::ZERO,
            in_flight: Vec::new(),
            noise: Noise(7),
            written: Vec::new(),
            wheel_m: TRAINER_WHEEL_M,
            real: REAL_RATIO,
        }
    }

    fn on_chain(mut self, real: f64) -> Self {
        self.real = real;
        self
    }

    fn remembering(mut self, chain: Option<f64>) -> Self {
        let router = std::mem::replace(&mut self.router, Router::new(Limits::FALLBACK, None));
        self.router = router.with_real_chain(chain);
        self
    }

    fn cadence(&self) -> f64 {
        self.v / (self.real * self.wheel_m) * 60.0
    }

    /// Speed of the virtual bike: the selected gear at the rider's cadence.
    fn virtual_kmh(&self) -> f64 {
        self.router
            .shifter()
            .map_or(0.0, |s| s.virtual_speed_kmh(self.cadence()))
    }

    /// Resistance the trainer applies now, W.
    fn resistance_w(&self) -> f64 {
        match &self.trainer {
            Trainer::Free => 0.0,
            Trainer::Erg(w) => *w,
            Trainer::Road(road) => road.power_w(self.v, MASS_KG),
        }
    }

    fn deliver(&mut self, command: Vec<u8>) {
        self.in_flight.push((self.t + WRITE_DELAY, command));
    }

    fn step(&mut self) {
        let now = self.t0 + self.t;
        // Control writes reaching the trainer.
        let (due, later): (Vec<_>, Vec<_>) =
            self.in_flight.drain(..).partition(|(at, _)| *at <= self.t);
        self.in_flight = later;
        for (_, c) in due {
            self.written.push((self.t, c[0]));
            match c[..] {
                [0x05, a, b] => self.trainer = Trainer::Erg(f64::from(i16::from_le_bytes([a, b]))),
                [0x11, ..] => {
                    if let Some(road) = Simulation::from_ftms(&c) {
                        self.trainer = Trainer::Road(road);
                    }
                }
                _ => {}
            }
        }
        // Flywheel: m·v·dv/dt = P_rider − P_trainer.
        let dt = STEP.as_secs_f64();
        let accel = (self.rider_w - self.resistance_w()) / (FLYWHEEL_KG * self.v.max(1.0));
        self.v = (self.v + accel * dt).max(0.0);
        if self.t >= self.next_telemetry {
            self.next_telemetry += Duration::from_millis(250);
            let kmh = (self.v * 3.6 + 0.3 * self.noise.next()).max(0.0);
            let rpm = (self.cadence() + 1.0 * self.noise.next()).max(0.0).round();
            let mut frame = vec![0x44, 0x00];
            frame.extend(((kmh * 100.0).round() as u16).to_le_bytes());
            frame.extend(((rpm * 2.0) as u16).to_le_bytes());
            frame.extend((self.resistance_w().round() as i16).to_le_bytes());
            self.router
                .trainer_notification(sig(INDOOR_BIKE_DATA), &frame, now);
        }
        if self.t >= self.next_tick {
            self.next_tick += Duration::from_millis(250);
            if let Some(c) = self.router.tick(now) {
                self.deliver(c);
            }
        }
        self.t += STEP;
    }

    fn ride(&mut self, seconds: f64) {
        let end = self.t + Duration::from_secs_f64(seconds);
        while self.t < end {
            self.step();
        }
    }

    /// Press like the bridge does: the new gear's command goes out at once.
    fn shift(&mut self, action: Action) -> bool {
        let moved = self.router.shift(action).is_some();
        if moved && let Some(c) = self.router.tick(self.t0 + self.t) {
            self.deliver(c);
        }
        moved
    }

    fn gear(&self) -> usize {
        self.router.shifter().map_or(0, |s| s.gear().number)
    }

    fn writes_since(&self, from: Duration) -> usize {
        self.written.iter().filter(|(t, _)| *t >= from).count()
    }
}

/// Road speed for `watts` on a flat road, by bisection.
fn flat_speed_kmh(watts: f64) -> f64 {
    let road = Simulation::from_ftms(&road(0.0))
        .map(|r| Simulation {
            grade_percent: 0.0,
            ..r
        })
        .unwrap_or_else(|| panic!("road"));
    let (mut lo, mut hi) = (0.0, 30.0);
    for _ in 0..60 {
        let mid = (lo + hi) / 2.0;
        if road.power_w(mid, MASS_KG) < watts {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo * 3.6
}

#[test]
fn road_feel_learns_the_real_chain_and_rides_like_a_bike() {
    let mut ride = Ride::new(ShiftFeel::Road);
    // Power targets until the real chain is known; noisy, whole-rpm telemetry must do.
    let mut learned_at = None;
    while ride.t < Duration::from_secs(20) {
        ride.step();
        if learned_at.is_none() && ride.router.real_chain().is_some() {
            learned_at = Some(ride.t);
        }
    }
    let learned_at = learned_at.unwrap_or_else(|| panic!("real chain not learned in 20 s"));
    let chain = ride.router.real_chain().unwrap_or_default();
    // The trainer's own wheel size is folded in: what matters is speed per crank turn.
    let expected = REAL_RATIO * TRAINER_WHEEL_M / 2.105;
    println!("real chain learned after {learned_at:?}: {chain:.3} (expected {expected:.3})");
    assert!(learned_at < Duration::from_secs(10), "{learned_at:?}");
    assert!(
        (chain / expected - 1.0).abs() < 0.02,
        "{chain} vs {expected}"
    );
    assert!(ride.router.road_feel_active());

    // At a set power a real bike's speed does not depend on the gear; only the cadence does.
    let speed = flat_speed_kmh(ride.rider_w);
    println!("200 W on the flat: {speed:.1} km/h on a real bike");
    while ride.shift(Action::ShiftDown) {}
    let mut cadences = vec![];
    loop {
        ride.ride(25.0);
        let (gear, kmh, rpm) = (ride.gear(), ride.virtual_kmh(), ride.cadence());
        println!("gear {gear:2}: {kmh:5.1} km/h at {rpm:5.1} rpm");
        if (40.0..=130.0).contains(&rpm) {
            assert!((kmh / speed - 1.0).abs() < 0.03, "gear {gear}: {kmh} km/h");
        }
        cadences.push(rpm);
        if !ride.shift(Action::ShiftUp) {
            break;
        }
    }
    assert!(
        cadences.windows(2).all(|w| w[1] < w[0]),
        "harder gears turn slower"
    );
    assert_eq!(
        ride.router.real_chain(),
        Some(chain),
        "virtual shifts never move the learned chain"
    );
}

#[test]
fn a_shift_is_felt_at_once_and_steady_riding_is_quiet() {
    let mut ride = Ride::new(ShiftFeel::Road);
    ride.ride(15.0);
    assert!(ride.router.road_feel_active());
    ride.ride(20.0);
    // Steady riding: no load changes, so (almost) nothing to send.
    let quiet_from = ride.t;
    ride.ride(30.0);
    let quiet = ride.writes_since(quiet_from);
    println!("steady 30 s: {quiet} writes");
    assert!(quiet <= 2, "{quiet} writes while riding steadily");

    // Shift up: the resistance at the same wheel speed rises once the write lands.
    let before = ride.resistance_w();
    let pressed = ride.t;
    assert!(ride.shift(Action::ShiftUp));
    let mut felt = None;
    while ride.t < pressed + Duration::from_secs(2) {
        ride.step();
        if felt.is_none() && ride.resistance_w() > before * 1.05 {
            felt = Some(ride.t - pressed);
        }
    }
    let felt = felt.unwrap_or_else(|| panic!("shift not felt within 2 s"));
    println!("shift felt after {felt:?}");
    assert!(felt <= WRITE_DELAY + Duration::from_millis(20), "{felt:?}");
}

#[test]
fn climbs_and_descents_follow_the_road_in_every_gear() {
    let mut ride = Ride::new(ShiftFeel::Road);
    ride.ride(15.0);
    for grade in [5.0, -4.0, 8.0, 0.0] {
        ride.router.app_write(&road(grade));
        ride.ride(40.0);
        let kmh = ride.virtual_kmh();
        let sim = Simulation::from_ftms(&road(grade)).unwrap_or_else(|| panic!("road"));
        let real = Simulation {
            grade_percent: grade,
            ..sim
        };
        let needed = real.power_w(kmh / 3.6, MASS_KG);
        println!(
            "grade {grade:+.0} %: {kmh:5.1} km/h at {:5.1} rpm, road needs {needed:5.0} W",
            ride.cadence()
        );
        if grade >= 0.0 {
            // Settled: the rider's 200 W hold the road's speed for the shown grade.
            assert!(
                (needed / ride.rider_w - 1.0).abs() < 0.05,
                "{grade}: {needed} W"
            );
        } else {
            // Downhill the road pulls: faster than on the flat for the same effort.
            assert!(kmh > flat_speed_kmh(ride.rider_w));
        }
    }
}

#[test]
fn power_feel_still_reaches_the_same_speed() {
    let mut ride = Ride::new(ShiftFeel::Power);
    ride.ride(60.0);
    let kmh = ride.virtual_kmh();
    println!("power feel, 200 W: {kmh:.1} km/h");
    assert!(!ride.router.road_feel_active());
    assert!((kmh / flat_speed_kmh(ride.rider_w) - 1.0).abs() < 0.03);
    assert!(ride.written.iter().all(|(_, op)| *op == 0x05));
}

#[test]
fn moving_the_real_chain_is_picked_up() {
    let mut ride = Ride::new(ShiftFeel::Road);
    ride.ride(15.0);
    let first = ride.router.real_chain().unwrap_or_default();
    // The rider shifts on the bike: same trainer speed now takes a different cadence.
    // A cog 25 % smaller: each crank turn moves the trainer 25 % further.
    ride.wheel_m *= 1.25;
    ride.ride(20.0);
    let second = ride.router.real_chain().unwrap_or_default();
    println!("real chain {first:.3} -> {second:.3}");
    assert!((second / first - 1.25).abs() < 0.03, "{first} -> {second}");
}

/// Replays a real trainer capture (bikectl monitor JSON lines) through the router:
/// `OSMOL_CAPTURE=ride.jsonl cargo test -p bike-session --test ride -- --ignored --nocapture`.
#[test]
#[ignore = "needs a local capture in OSMOL_CAPTURE"]
fn replay_capture() {
    let Some(path) = std::env::var_os("OSMOL_CAPTURE") else {
        panic!("set OSMOL_CAPTURE to a capture file");
    };
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{e}"));
    let drivetrain = Drivetrain::new(&[50, 34], &[11, 12, 13, 14, 15, 17, 19, 21, 24, 28], 2136)
        .unwrap_or_else(|e| panic!("{e}"));
    let shifter = VirtualShifter::new(drivetrain, MASS_KG).with_grade_scale(2.0);
    let mut router = Router::new(Limits::FALLBACK, Some(shifter));
    router.app_write(&road(1.0));
    let t0 = Instant::now();
    let mut first_ms = None;
    let (mut frames, mut writes, mut learned) = (0, 0, None);
    let mut chains = vec![];
    let mut pedalled = false;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line).unwrap_or_default();
        if v["characteristic"].as_str() != Some(&sig(INDOOR_BIKE_DATA).to_string()) {
            continue;
        }
        let (Some(ms), Some(raw)) = (v["timestamp_ms"].as_u64(), v["raw_hex"].as_str()) else {
            continue;
        };
        let at = Duration::from_millis(ms - *first_ms.get_or_insert(ms));
        let frame = bike_protocol::unhex(raw).unwrap_or_default();
        pedalled |= v["value"]["cadence_rpm"]
            .as_f64()
            .is_some_and(|c| c >= 40.0);
        router.trainer_notification(sig(INDOOR_BIKE_DATA), &frame, t0 + at);
        frames += 1;
        if let Some(c) = router.tick(t0 + at) {
            writes += 1;
            if c[0] == 0x11 {
                chains.push(router.real_chain());
            }
        }
        if learned.is_none() && router.real_chain().is_some() {
            learned = Some(at);
        }
        if frames % 40 == 0 && router.shift(Action::ShiftUp).is_none() {
            while router.shift(Action::ShiftDown).is_some() {}
        }
    }
    chains.dedup();
    println!(
        "{frames} frames, {writes} writes, chain learned after {learned:?}, chain values {chains:?}"
    );
    if pedalled {
        assert!(learned.is_some(), "never learned the real chain");
        assert_eq!(chains.len(), 1, "the learned chain moved: {chains:?}");
    } else {
        // Hand spinning or standing still: nothing to learn, and nothing is guessed.
        assert_eq!(learned, None);
    }
}

#[test]
fn a_remembered_chain_gives_road_feel_from_the_first_stroke() {
    let expected = REAL_RATIO * TRAINER_WHEEL_M / 2.105;
    let mut ride = Ride::new(ShiftFeel::Road).remembering(Some(expected));
    assert!(ride.router.road_feel_active());
    ride.ride(1.0);
    assert!(
        ride.written.iter().all(|(_, op)| *op == 0x11),
        "no power targets"
    );
    ride.ride(30.0);
    assert_eq!(
        ride.router.real_chain(),
        Some(expected),
        "a right guess stays"
    );

    // Moved on the bike since the last ride: corrected after some steady pedalling.
    let mut moved = Ride::new(ShiftFeel::Road)
        .on_chain(50.0 / 14.0)
        .remembering(Some(expected));
    moved.ride(20.0);
    let now = moved.router.real_chain().unwrap_or_default();
    let truth = 50.0 / 14.0 * TRAINER_WHEEL_M / 2.105;
    println!("remembered {expected:.3}, corrected to {now:.3} (truth {truth:.3})");
    assert!((now / truth - 1.0).abs() < 0.02);
}

/// Chain on 34x28 on the trainer: the hard virtual gears need more air drag than FTMS can carry,
/// so part of it rides in the grade. Speed must still not depend on the gear.
#[test]
fn an_easy_real_chain_still_rides_true() {
    let mut ride = Ride::new(ShiftFeel::Road).on_chain(34.0 / 28.0);
    ride.ride(15.0);
    assert!(ride.router.road_feel_active());
    let speed = flat_speed_kmh(ride.rider_w);
    while ride.shift(Action::ShiftDown) {}
    for _ in 0..12 {
        assert!(ride.shift(Action::ShiftUp));
    }
    loop {
        ride.ride(25.0);
        let from = ride.t;
        ride.ride(10.0);
        let (gear, kmh, rpm) = (ride.gear(), ride.virtual_kmh(), ride.cadence());
        let writes = ride.writes_since(from);
        println!(
            "34x28 real, gear {gear:2}: {kmh:5.1} km/h at {rpm:5.1} rpm, {writes} writes in 10 s"
        );
        assert!((kmh / speed - 1.0).abs() < 0.03, "gear {gear}: {kmh} km/h");
        assert!(writes <= 5, "gear {gear}: {writes} writes in 10 s");
        if !ride.shift(Action::ShiftUp) {
            break;
        }
    }
}
