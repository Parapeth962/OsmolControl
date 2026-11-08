//! Virtual trainer: mirror the real trainer over DirCon, relay filtered control commands and
//! optionally replace the app's road simulation with gear-aware power targets.
use anyhow::Result;
use bike_ble::trainer::Snapshot;
use bike_dircon::{Characteristic, Gatt, Service, gatt, sig};
use bike_gearing::{Gear, RealChain, Simulation, VirtualShifter};
use bike_protocol::Action;
use bike_protocol::ftms_control::{self, Verdict};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, watch};
use uuid::Uuid;

const CONTROL_POINT: u16 = 0x2ad9;
const INDOOR_BIKE_DATA: u16 = 0x2ad2;
const MACHINE_STATUS: u16 = 0x2ada;
/// Power target update period in virtual-shifting mode (the trainer queue paces writes too);
/// matches the trainer's 4 Hz cadence reports. Shifts send a target at once.
const TICK: Duration = Duration::from_millis(250);

/// The DirCon GATT table: the trainer's FTMS and Cycling Power characteristics with their real
/// values. Only the FTMS Control Point is writable; other control points are not exposed.
pub fn mirror(snapshot: &Snapshot) -> Gatt {
    let mut services: Vec<Service> = vec![];
    for c in &snapshot.characteristics {
        if c.writable && c.uuid != sig(CONTROL_POINT) {
            continue;
        }
        let properties = [
            (c.readable, gatt::READ),
            (c.writable, gatt::WRITE),
            (c.notify, gatt::NOTIFY),
            (c.indicate, gatt::INDICATE),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .fold(0, |bits, (_, bit)| bits | bit);
        let characteristic = Characteristic {
            uuid: c.uuid,
            properties,
            value: c.value.clone(),
        };
        match services.iter_mut().find(|s| s.uuid == c.service) {
            Some(s) => s.characteristics.push(characteristic),
            None => services.push(Service {
                uuid: c.service,
                characteristics: vec![characteristic],
            }),
        }
    }
    Gatt { services }
}

/// Whether the trainer advertises Indoor Bike Simulation (FTMS Feature 2ACC: target setting
/// features in bytes 4..8, bit 13). An unreadable feature table counts as yes; a trainer that
/// then refuses the road is switched to power targets.
pub fn simulates_road(snapshot: &Snapshot) -> bool {
    let Some(feature) = snapshot
        .characteristics
        .iter()
        .find(|c| c.uuid == sig(0x2acc))
    else {
        return true;
    };
    match feature.value.get(4..8) {
        Some(&[a, b, c, d]) => u32::from_le_bytes([a, b, c, d]) & (1 << 13) != 0,
        _ => true,
    }
}

pub const HEART_RATE_MEASUREMENT: Uuid = sig(0x2a37);

/// Heart Rate service carrying the strap's measurements unchanged.
pub fn heart_rate_service() -> Service {
    Service {
        uuid: sig(0x180d),
        characteristics: vec![Characteristic {
            uuid: HEART_RATE_MEASUREMENT,
            properties: gatt::NOTIFY,
            value: vec![],
        }],
    }
}

use crate::{Control as Mode, GearInfo, ShiftFeel, StatusTx};

/// Road changes smaller than this share of the load are not sent: below what a rider feels
/// (every gear step is several times larger), and they would only rock the load with sensor noise.
const LOAD_DEADBAND: f64 = 0.015;
/// Speed the load is compared at when the trainer is slower, so a shift while standing still
/// is sent too.
const MIN_COMPARE_MPS: f64 = 2.0;

/// Whether `next` would ask the trainer for what `last` already did, as felt at `speed_mps`.
fn same_command(last: &[u8], next: &[u8], speed_mps: f64, mass_kg: f64) -> bool {
    match (Simulation::from_ftms(last), Simulation::from_ftms(next)) {
        (Some(a), Some(b)) => {
            let v = speed_mps.max(MIN_COMPARE_MPS);
            let (pa, pb) = (a.power_w(v, mass_kg), b.power_w(v, mass_kg));
            (pa - pb).abs() <= LOAD_DEADBAND * pa.max(pb) + 0.2
        }
        _ => last == next,
    }
}

/// Cadence older than this is treated as unknown (the rider stopped or the trainer went quiet).
const CADENCE_MAX_AGE: Duration = crate::TELEMETRY_MAX_AGE;
/// Cadence readings kept for the median: one odd reading (a 0 or a spike between two normal
/// ones) is ignored instead of moving the power target, which rises roughly with its cube.
const CADENCE_SAMPLES: usize = 3;
/// Trainer speed readings averaged for the road sent to it (2 s at 4 Hz). The speed only matters
/// there for air drag beyond what FTMS can carry, which changes slowly; averaging keeps sensor
/// noise from rocking the load.
const SPEED_SAMPLES: usize = 8;

/// Pure routing between the app, the trainer and the virtual drivetrain.
pub struct Router {
    limits: ftms_control::Limits,
    shifter: Option<VirtualShifter>,
    feel: ShiftFeel,
    /// The rider's real gear on the trainer; known once they pedal steadily.
    chain: Option<RealChain>,
    mode: Mode,
    /// Latest cadence readings, oldest first.
    cadence: VecDeque<(f64, Instant)>,
    /// Latest trainer wheel speeds, km/h, oldest first.
    speed: VecDeque<(f64, Instant)>,
    last_tick: Option<Instant>,
    /// The last gear command sent, so an unchanged one is not repeated.
    last_sent: Option<Vec<u8>>,
}

/// Outcome of an app write: commands for the trainer and notifications for the app.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Routed {
    pub to_trainer: Vec<Vec<u8>>,
    pub to_app: Vec<(Uuid, Vec<u8>)>,
}

impl Router {
    /// Gears are felt as a road (see [`ShiftFeel::Road`]) unless [`Self::with_feel`] says otherwise.
    pub fn new(limits: ftms_control::Limits, shifter: Option<VirtualShifter>) -> Self {
        let chain = shifter
            .as_ref()
            .map(|s| RealChain::new(s.drivetrain().wheel_circumference_mm));
        Self {
            limits,
            shifter,
            feel: ShiftFeel::Road,
            chain,
            mode: Mode::None,
            cadence: VecDeque::with_capacity(CADENCE_SAMPLES),
            speed: VecDeque::with_capacity(SPEED_SAMPLES),
            last_tick: None,
            last_sent: None,
        }
    }

    pub fn with_feel(mut self, feel: ShiftFeel) -> Self {
        self.feel = feel;
        self
    }

    /// Start from the real chain learned on an earlier ride, so the road is felt from the first
    /// pedal stroke. It is still checked while riding.
    pub fn with_real_chain(mut self, ratio: Option<f64>) -> Self {
        self.chain = self.chain.map(|c| c.with_ratio(ratio));
        self
    }

    pub fn shifter(&self) -> Option<&VirtualShifter> {
        self.shifter.as_ref()
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn feel(&self) -> ShiftFeel {
        self.feel
    }

    /// The rider's real gear on the trainer (chainring/cog), once learned.
    pub fn real_chain(&self) -> Option<f64> {
        self.chain.as_ref()?.ratio()
    }

    /// [`Self::real_chain`] as metres per crank revolution.
    pub fn real_chain_m(&self) -> Option<f64> {
        let wheel = self.shifter.as_ref()?.drivetrain().wheel_circumference_mm;
        Some(self.real_chain()? * f64::from(wheel) / 1000.0)
    }

    /// Whether the trainer simulates the road for the gear itself (rather than holding a
    /// power target computed from the cadence).
    pub fn road_feel_active(&self) -> bool {
        self.feel == ShiftFeel::Road && self.shifter.is_some() && self.real_chain().is_some()
    }

    pub fn gear_info(&self) -> Option<GearInfo> {
        let s = self.shifter.as_ref()?;
        Some(GearInfo::new(&s.gear(), s.gear_count()))
    }

    pub fn app_write(&mut self, data: &[u8]) -> Routed {
        let command = match ftms_control::filter(data, &self.limits) {
            Verdict::Forward(command) => command,
            Verdict::Reject(response) => {
                tracing::warn!(command = %bike_protocol::hex(data), "control command refused");
                return Routed {
                    to_app: vec![(sig(CONTROL_POINT), response.to_vec())],
                    ..Routed::default()
                };
            }
        };
        let op = command[0];
        if op == ftms_control::SET_SIMULATION
            && let Some(shifter) = &mut self.shifter
            && let Some(simulation) = Simulation::from_ftms(&command)
        {
            // Consumed here: the trainer receives the gears' road or power targets instead.
            shifter.set_simulation(simulation);
            self.mode = Mode::Simulation;
            let ok = vec![ftms_control::RESPONSE, op, ftms_control::RESULT_SUCCESS];
            return Routed {
                to_app: vec![(sig(CONTROL_POINT), ok)],
                ..Routed::default()
            };
        }
        match op {
            ftms_control::SET_SIMULATION => self.mode = Mode::Simulation,
            ftms_control::SET_TARGET_POWER | ftms_control::SET_TARGET_RESISTANCE => {
                self.enter(Mode::Target)
            }
            ftms_control::RESET => self.enter(Mode::None),
            // Request Control, Start or Stop from the app: some trainers drop their target on
            // these, so the gears' command is sent again instead of waiting for it to change.
            _ => self.last_sent = None,
        }
        tracing::debug!(command = %bike_protocol::hex(&command), "control command relayed");
        Routed {
            to_trainer: vec![command],
            ..Routed::default()
        }
    }

    fn enter(&mut self, mode: Mode) {
        self.mode = mode;
        self.last_sent = None;
        if let Some(s) = &mut self.shifter {
            s.clear_simulation();
        }
    }

    /// Track cadence, speed and the real chain; hide trainer replies to OsmolControl's own
    /// commands from the app. A trainer that refuses the gears' road gets power targets instead.
    pub fn trainer_notification(&mut self, uuid: Uuid, value: &[u8], now: Instant) -> bool {
        if uuid == sig(INDOOR_BIKE_DATA)
            && let Ok(v) = bike_protocol::indoor_bike(value)
        {
            let num = |k: &str| v.get(k).and_then(serde_json::Value::as_f64);
            if let Some(rpm) = num("cadence_rpm") {
                if self.cadence.len() == CADENCE_SAMPLES {
                    self.cadence.pop_front();
                }
                self.cadence.push_back((rpm, now));
            }
            if let Some(kmh) = num("speed_kmh") {
                if self.speed.len() == SPEED_SAMPLES {
                    self.speed.pop_front();
                }
                self.speed.push_back((kmh, now));
            }
            if let (Some(chain), Some(kmh), Some(rpm)) =
                (&mut self.chain, num("speed_kmh"), num("cadence_rpm"))
            {
                chain.observe(kmh, rpm);
            }
        }
        let injected = self.mode == Mode::Simulation && self.shifter.is_some();
        let own_reply = uuid == sig(CONTROL_POINT)
            && value.first() == Some(&ftms_control::RESPONSE)
            && matches!(
                value.get(1),
                Some(&(ftms_control::SET_TARGET_POWER | ftms_control::SET_SIMULATION))
            );
        if injected
            && own_reply
            && value.get(1) == Some(&ftms_control::SET_SIMULATION)
            && value.get(2) != Some(&ftms_control::RESULT_SUCCESS)
            && self.feel == ShiftFeel::Road
        {
            tracing::warn!(reply = %bike_protocol::hex(value), "trainer refused the gears' road");
            self.feel = ShiftFeel::Power;
            self.last_sent = None;
        }
        // FTMS Machine Status 0x08 = "target power changed", 0x12 = "simulation changed".
        let own_status =
            uuid == sig(MACHINE_STATUS) && matches!(value.first(), Some(&(0x08 | 0x12)));
        !(injected && (own_reply || own_status))
    }

    /// Periodic gear command in virtual-shifting mode: the road for the trainer to simulate,
    /// or a power target until the real chain is known (or with [`ShiftFeel::Power`]).
    pub fn tick(&mut self, now: Instant) -> Option<Vec<u8>> {
        let dt = self
            .last_tick
            .map_or(0.0, |t| now.duration_since(t).as_secs_f64());
        self.last_tick = Some(now);
        if self.mode != Mode::Simulation {
            return None;
        }
        let command = match self.road_command(now) {
            Some(command) => command,
            None => self.power_command(now, dt)?,
        };
        let mass = self.shifter.as_ref().map_or(0.0, VirtualShifter::mass_kg);
        let speed = self.trainer_speed(now);
        if self
            .last_sent
            .as_deref()
            .is_some_and(|last| same_command(last, &command, speed, mass))
        {
            return None;
        }
        self.last_sent = Some(command.clone());
        Some(command)
    }

    fn road_command(&self, now: Instant) -> Option<Vec<u8>> {
        if !self.road_feel_active() {
            return None;
        }
        let speed = self.trainer_speed(now);
        let road = self
            .shifter
            .as_ref()?
            .trainer_road(self.real_chain()?, speed)?;
        match ftms_control::filter(&road.to_ftms(), &self.limits) {
            Verdict::Forward(command) => Some(command),
            Verdict::Reject(_) => None,
        }
    }

    /// Average of the fresh trainer speed readings, m/s; 0 without any.
    fn trainer_speed(&self, now: Instant) -> f64 {
        let fresh: Vec<f64> = self
            .speed
            .iter()
            .filter(|(_, at)| now.saturating_duration_since(*at) <= CADENCE_MAX_AGE)
            .map(|(kmh, _)| *kmh)
            .collect();
        if fresh.is_empty() {
            0.0
        } else {
            fresh.iter().sum::<f64>() / fresh.len() as f64 / 3.6
        }
    }

    fn power_command(&mut self, now: Instant, dt: f64) -> Option<Vec<u8>> {
        let cadence = self.cadence(now);
        let target = self.shifter.as_mut()?.update(cadence, dt)?;
        // Bounded by the clamp and the trainer range below; rounding to whole watts.
        let watts = target.round().clamp(0.0, f64::from(i16::MAX)) as i16;
        let [a, b] = watts.to_le_bytes();
        match ftms_control::filter(&[ftms_control::SET_TARGET_POWER, a, b], &self.limits) {
            Verdict::Forward(command) => Some(command),
            Verdict::Reject(_) => None,
        }
    }

    /// Median of the fresh cadence readings; `None` once the trainer has not reported cadence
    /// for [`CADENCE_MAX_AGE`].
    pub fn cadence(&self, now: Instant) -> Option<f64> {
        let mut fresh: Vec<f64> = self
            .cadence
            .iter()
            .filter(|(_, at)| now.saturating_duration_since(*at) <= CADENCE_MAX_AGE)
            .map(|(rpm, _)| *rpm)
            .collect();
        fresh.sort_by(f64::total_cmp);
        fresh.get(fresh.len() / 2).copied()
    }

    /// Whether the trainer has reported cadence at all during this ride.
    fn ever_had_cadence(&self) -> bool {
        !self.cadence.is_empty()
    }

    /// The last target did not reach the trainer: send it again on the next tick.
    fn forget_sent(&mut self) {
        self.last_sent = None;
    }

    /// Returns the new gear if it changed.
    pub fn shift(&mut self, action: Action) -> Option<Gear> {
        let shifter = self.shifter.as_mut()?;
        let harder = match action {
            Action::ShiftUp => true,
            Action::ShiftDown => false,
        };
        let moved = shifter.shift(harder);
        if moved {
            // Every shift reaches the trainer, however close the next gear's ratio is.
            self.last_sent = None;
        }
        moved.then(|| shifter.gear())
    }
}

pub struct Channels {
    pub app_writes: mpsc::Receiver<(Uuid, Vec<u8>)>,
    pub from_trainer: broadcast::Receiver<(Uuid, Vec<u8>)>,
    pub to_app: broadcast::Sender<(Uuid, Vec<u8>)>,
    pub to_trainer: mpsc::Sender<Vec<u8>>,
    pub actions: Option<broadcast::Receiver<(Instant, Action)>>,
    pub status: StatusTx,
}

/// Publish what the router knows: control mode, road grade, power target and gear.
fn publish(router: &Router, status: &StatusTx, grade: Option<f64>, target: Option<i16>) {
    status.send_modify(|s| {
        s.control = router.mode();
        if grade.is_some() {
            s.grade_percent = grade;
        }
        if target.is_some() {
            s.target_w = target;
        }
        s.gear = router.gear_info();
        s.real_chain = router.real_chain();
        s.real_chain_m = router.real_chain_m();
        s.road_feel = router.road_feel_active();
        s.shift_feel = router.feel();
    });
}

fn telemetry(status: &StatusTx, value: &[u8]) {
    let Ok(v) = bike_protocol::indoor_bike(value) else {
        return;
    };
    let num = |k: &str| v.get(k).and_then(serde_json::Value::as_f64);
    status.send_modify(|s| {
        s.power_w = v
            .get("power_w")
            .and_then(serde_json::Value::as_i64)
            .and_then(|w| i16::try_from(w).ok());
        s.cadence_rpm = num("cadence_rpm");
        s.speed_kmh = num("speed_kmh");
        s.telemetry_at = Some(Instant::now());
    });
}

pub async fn run(
    mut router: Router,
    mut ch: Channels,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    publish(&router, &ch.status, None, None);
    let mut ticker = tokio::time::interval(TICK);
    let mut cadence_lost = false;
    let mut told = Told::default();
    loop {
        tokio::select! {
            _ = stop.changed() => return Ok(()),
            w = ch.app_writes.recv() => {
                let Some((uuid, data)) = w else { return Ok(()) };
                if uuid != sig(CONTROL_POINT) {
                    continue;
                }
                let grade = Simulation::from_ftms(&data).map(|s| s.grade_percent);
                let target = match data[..] {
                    [ftms_control::SET_TARGET_POWER, a, b] => Some(i16::from_le_bytes([a, b])),
                    _ => None,
                };
                let before = router.mode();
                let routed = router.app_write(&data);
                // With virtual shifting, show the grade actually ridden (scaled).
                let grade = grade
                    .and(router.shifter().and_then(VirtualShifter::grade_percent))
                    .or(grade);
                tracing::debug!(command = %bike_protocol::hex(&data), ?grade, "app control write");
                if router.mode() != before && router.shifter().is_some() {
                    crate::log(&ch.status, match router.mode() {
                        Mode::Simulation => "MyWhoosh sends the road again; gears active.",
                        Mode::Target => "MyWhoosh set power or resistance itself; gears paused.",
                        Mode::None => "MyWhoosh released the trainer; gears paused.",
                    });
                }
                let relayed = !routed.to_trainer.is_empty();
                for c in routed.to_trainer {
                    send(&ch.to_trainer, c);
                }
                for n in routed.to_app {
                    let _ = ch.to_app.send(n);
                }
                publish(&router, &ch.status, grade, target);
                if relayed && router.mode() == Mode::Simulation && router.shifter().is_some() {
                    // Queued behind the relayed command, so the trainer ends on the gears' target.
                    power_target(&mut router, &ch);
                }
            }
            n = ch.from_trainer.recv() => match n {
                Ok((uuid, value)) => {
                    if uuid == sig(INDOOR_BIKE_DATA) {
                        telemetry(&ch.status, &value);
                    }
                    if router.trainer_notification(uuid, &value, Instant::now()) {
                        let _ = ch.to_app.send((uuid, value));
                    }
                    feel_news(&router, &ch.status, &mut told);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            a = recv_action(&mut ch.actions) => {
                if let Some(gear) = a.and_then(|action| router.shift(action)) {
                    let of = router.shifter().map_or(0, VirtualShifter::gear_count);
                    crate::log(&ch.status, format!("Gear {}/{of} ({}x{})", gear.number, gear.chainring, gear.cog));
                    publish(&router, &ch.status, None, None);
                    // Do not wait for the next tick: the trainer gets the new gear's power now.
                    power_target(&mut router, &ch);
                    ticker.reset();
                }
            }
            _ = ticker.tick() => {
                power_target(&mut router, &ch);
                // The trainer simulating the road needs no cadence; power targets do.
                let lost = router.mode() == Mode::Simulation
                    && !router.road_feel_active()
                    && router.ever_had_cadence()
                    && router.cadence(Instant::now()).is_none();
                if lost != cadence_lost && router.shifter().is_some() {
                    cadence_lost = lost;
                    crate::log(&ch.status, if lost {
                        "No cadence from the trainer for 3 s; resistance eased until it is back."
                    } else {
                        "Cadence from the trainer is back."
                    });
                }
            }
        }
    }
}

/// What the log has said about how the gears drive the trainer.
#[derive(Default)]
struct Told {
    chain: Option<f64>,
    feel: Option<ShiftFeel>,
}

fn feel_news(router: &Router, status: &StatusTx, told: &mut Told) {
    if router.shifter().is_none() {
        return;
    }
    if told.feel == Some(ShiftFeel::Road) && router.feel() == ShiftFeel::Power {
        crate::log(
            status,
            "The trainer refused the simulated road; gears use power targets instead.",
        );
        status.send_modify(|s| {
            s.shift_feel = ShiftFeel::Power;
            s.road_feel = false;
        });
    }
    told.feel = Some(router.feel());
    let chain = router.real_chain();
    if router.feel() == ShiftFeel::Road
        && let Some(ratio) = chain
        && chain != told.chain
    {
        crate::log(
            status,
            if told.chain.is_none() {
                format!(
                    "Real gear on the trainer: ratio {ratio:.2}. The trainer now simulates the \
                     road for each virtual gear, so shifts are felt at once."
                )
            } else {
                format!("Real gear on the trainer changed: ratio {ratio:.2}.")
            },
        );
        if let Some(hint) = router.shifter().and_then(|s| chain_hint(s, ratio)) {
            crate::log(status, hint);
        }
        status.send_modify(|s| {
            s.real_chain = chain;
            s.real_chain_m = router.real_chain_m();
            s.road_feel = router.road_feel_active();
        });
    }
    told.chain = chain;
}

/// How far the real chain may sit from the middle of the virtual range before the ends of the
/// range suffer: the trainer's grade limit caps the hardest gears, its minimum resistance the
/// easiest ones.
const CHAIN_SPREAD: f64 = 1.5;

/// Advice when the real chain sits far from the middle of the virtual gears.
fn chain_hint(shifter: &VirtualShifter, real: f64) -> Option<String> {
    let gears = shifter.drivetrain().gears();
    let (first, last) = (gears.first()?, gears.last()?);
    let middle = (first.ratio * last.ratio).sqrt();
    let side = if real < middle / CHAIN_SPREAD {
        "an easy"
    } else if real > middle * CHAIN_SPREAD {
        "a hard"
    } else {
        return None;
    };
    let example = gears.iter().min_by(|a, b| {
        (a.ratio - middle)
            .abs()
            .total_cmp(&(b.ratio - middle).abs())
    })?;
    Some(format!(
        "Your chain sits in {side} gear on the trainer (ratio {real:.2}). For the hardest and \
         easiest virtual gears to feel right, move it to a middle one, such as {}x{} \
         (ratio {:.2}), and leave it there.",
        example.chainring, example.cog, example.ratio
    ))
}

/// False when the command was dropped because the trainer link is backed up.
fn send(tx: &mpsc::Sender<Vec<u8>>, command: Vec<u8>) -> bool {
    let sent = tx.try_send(command).is_ok();
    if !sent {
        tracing::warn!("trainer busy; control command dropped");
    }
    sent
}

/// Send the router's next gear command (road or power target) to the trainer, if any.
fn power_target(router: &mut Router, ch: &Channels) {
    let Some(c) = router.tick(Instant::now()) else {
        return;
    };
    let target = match c[..] {
        [ftms_control::SET_TARGET_POWER, a, b] => Some(i16::from_le_bytes([a, b])),
        _ => None,
    };
    {
        let s = ch.status.borrow();
        tracing::debug!(
            command = %bike_protocol::hex(&c),
            power = ?s.power_w,
            cadence = ?s.cadence_rpm,
            grade = ?s.grade_percent,
            gear = ?s.gear.as_ref().map(|g| g.number),
            real_chain = ?router.real_chain(),
            "gear command"
        );
    }
    if !send(&ch.to_trainer, c) {
        // Not deduplicated against a command the trainer never got.
        router.forget_sent();
        return;
    }
    publish(router, &ch.status, None, target);
    if target.is_none() {
        // The trainer sets the power itself from the road.
        ch.status.send_modify(|s| s.target_w = None);
    }
}

/// Next fresh controller action; pends forever without a controller.
async fn recv_action(
    actions: &mut Option<broadcast::Receiver<(Instant, Action)>>,
) -> Option<Action> {
    let Some(rx) = actions else {
        return std::future::pending().await;
    };
    match rx.recv().await {
        Ok((at, action)) if at.elapsed() < Action::MAX_AGE => Some(action),
        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => None,
        Err(broadcast::error::RecvError::Closed) => {
            *actions = None;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bike_ble::trainer::MirroredCharacteristic;

    fn ch(
        service: u16,
        id: u16,
        readable: bool,
        writable: bool,
        notify: bool,
        indicate: bool,
    ) -> MirroredCharacteristic {
        MirroredCharacteristic {
            service: sig(service),
            uuid: sig(id),
            readable,
            writable,
            notify,
            indicate,
            value: vec![id as u8],
        }
    }

    #[test]
    fn mirror_keeps_ftms_cp_and_drops_other_control_points() {
        let snapshot = Snapshot {
            name: "t".into(),
            limits: ftms_control::Limits::FALLBACK,
            characteristics: vec![
                ch(0x1826, 0x2acc, true, false, false, false),
                ch(0x1826, 0x2ad2, false, false, true, false),
                ch(0x1826, 0x2ad9, false, true, false, true),
                ch(0x1818, 0x2a63, false, false, true, false),
                ch(0x1818, 0x2a66, false, true, false, true),
            ],
        };
        let g = mirror(&snapshot);
        assert_eq!(g.services.len(), 2);
        let ftms = &g.services[0].characteristics;
        assert_eq!(
            ftms.iter().map(|c| c.properties).collect::<Vec<_>>(),
            vec![gatt::READ, gatt::NOTIFY, gatt::WRITE | gatt::INDICATE]
        );
        assert_eq!(ftms[0].value, vec![0xcc]);
        assert!(
            g.services[1]
                .characteristics
                .iter()
                .all(|c| c.uuid != sig(0x2a66))
        );
    }

    #[test]
    fn heart_rate_service_only_notifies() {
        let s = heart_rate_service();
        assert_eq!(s.uuid, sig(0x180d));
        assert_eq!(s.characteristics.len(), 1);
        assert_eq!(s.characteristics[0].uuid, HEART_RATE_MEASUREMENT);
        assert_eq!(s.characteristics[0].properties, gatt::NOTIFY);
    }

    fn router(shifting: bool) -> Router {
        let shifter = shifting.then(|| {
            let t = &bike_gearing::TEMPLATES[0];
            VirtualShifter::new(t.drivetrain().unwrap_or_else(|e| panic!("{e}")), 84.0)
        });
        Router::new(ftms_control::Limits::FALLBACK, shifter)
    }
    const SIM: [u8; 7] = [0x11, 0, 0, 0xb3, 0, 0x35, 0x33];

    #[test]
    fn passthrough_without_shifting() {
        let mut r = router(false);
        assert_eq!(r.app_write(&[0x00]).to_trainer, vec![vec![0x00]]);
        assert_eq!(r.app_write(&SIM).to_trainer, vec![SIM.to_vec()]);
        assert_eq!(r.tick(Instant::now()), None);
        let refused = r.app_write(&[0x13]);
        assert!(refused.to_trainer.is_empty());
        assert_eq!(
            refused.to_app,
            vec![(sig(CONTROL_POINT), vec![0x80, 0x13, 0x02])]
        );
    }

    #[test]
    fn shifting_turns_simulation_into_power_targets() {
        let mut r = router(true);
        let t0 = Instant::now();
        let routed = r.app_write(&SIM);
        assert!(
            routed.to_trainer.is_empty(),
            "simulation must not reach the trainer"
        );
        assert_eq!(
            routed.to_app,
            vec![(sig(CONTROL_POINT), vec![0x80, 0x11, 0x01])]
        );
        // Captured X2 Max frame: 62 rpm.
        assert!(r.trainer_notification(
            sig(INDOOR_BIKE_DATA),
            &[0x44, 0, 0x9b, 0x07, 0x7c, 0, 0x52, 0],
            t0
        ));
        let first = r.tick(t0).unwrap_or_else(|| panic!("no target"));
        assert_eq!(first[0], 0x05);
        let low = i16::from_le_bytes([first[1], first[2]]);
        assert!(low > 0);
        assert!(r.shift(Action::ShiftUp).is_some());
        assert!(r.shift(Action::ShiftUp).is_some());
        let later = t0 + Duration::from_secs(5);
        r.trainer_notification(
            sig(INDOOR_BIKE_DATA),
            &[0x44, 0, 0x9b, 0x07, 0x7c, 0, 0x52, 0],
            later,
        );
        let next = r.tick(later).unwrap_or_else(|| panic!("no target"));
        assert!(i16::from_le_bytes([next[1], next[2]]) > low);
        // The trainer's reply to our own target is hidden from the app.
        assert!(!r.trainer_notification(sig(CONTROL_POINT), &[0x80, 0x05, 0x01], later));
        assert!(r.trainer_notification(sig(CONTROL_POINT), &[0x80, 0x00, 0x01], later));
    }

    #[test]
    fn every_claris_shift_reaches_the_trainer_at_once() {
        let t = bike_gearing::template("claris-r2000-50-34-11-32")
            .unwrap_or_else(|| panic!("no Claris template"));
        let d = t.drivetrain().unwrap_or_else(|e| panic!("{e}"));
        let mut r = Router::new(
            ftms_control::Limits::FALLBACK,
            Some(VirtualShifter::new(d, 84.0)),
        );
        let t0 = Instant::now();
        r.app_write(&SIM);
        r.trainer_notification(
            sig(INDOOR_BIKE_DATA),
            &[0x44, 0, 0x9b, 0x07, 0x7c, 0, 0x52, 0],
            t0,
        );
        let watts = |c: Vec<u8>| i16::from_le_bytes([c[1], c[2]]);
        while r.shift(Action::ShiftDown).is_some() {}
        let mut last = r.tick(t0).map(watts).unwrap_or_else(|| panic!("no target"));
        let mut gears = vec![1];
        // No time passes between shifts: the slew limit must not hold the new gear back.
        while let Some(g) = r.shift(Action::ShiftUp) {
            let now = r
                .tick(t0)
                .map(watts)
                .unwrap_or_else(|| panic!("gear {}", g.number));
            assert!(now > last, "gear {}: {now} W <= {last} W", g.number);
            last = now;
            gears.push(g.number);
        }
        assert_eq!(gears, (1..=16).collect::<Vec<_>>());
        while let Some(g) = r.shift(Action::ShiftDown) {
            let now = r
                .tick(t0)
                .map(watts)
                .unwrap_or_else(|| panic!("gear {}", g.number));
            assert!(now < last, "gear {}: {now} W >= {last} W", g.number);
            last = now;
        }
        assert_eq!(
            r.gear_info().map(|g| (g.number, g.chainring, g.cog)),
            Some((1, 34, 32))
        );
    }

    #[test]
    fn erg_from_app_overrides_gears_and_stale_cadence_goes_easy() {
        let mut r = router(true);
        let t0 = Instant::now();
        r.app_write(&SIM);
        r.trainer_notification(
            sig(INDOOR_BIKE_DATA),
            &[0x44, 0, 0x9b, 0x07, 0x7c, 0, 0x52, 0],
            t0,
        );
        assert!(r.tick(t0).is_some());
        let stale = t0 + Duration::from_secs(60);
        let easy = r.tick(stale).unwrap_or_else(|| panic!("no target"));
        assert_eq!(i16::from_le_bytes([easy[1], easy[2]]), 0);
        assert_eq!(
            r.app_write(&[0x05, 0xc8, 0]).to_trainer,
            vec![vec![0x05, 0xc8, 0]]
        );
        assert_eq!(
            r.tick(stale + Duration::from_secs(1)),
            None,
            "ERG owns the trainer"
        );
        assert!(r.trainer_notification(sig(CONTROL_POINT), &[0x80, 0x05, 0x01], stale));
    }

    /// Captured X2 Max Indoor Bike Data at 62 rpm.
    const PEDALLING: [u8; 8] = [0x44, 0, 0x9b, 0x07, 0x7c, 0, 0x52, 0];
    fn watts(command: Option<Vec<u8>>) -> i16 {
        let c = command.unwrap_or_else(|| panic!("no power target"));
        assert_eq!(c[0], ftms_control::SET_TARGET_POWER);
        i16::from_le_bytes([c[1], c[2]])
    }

    #[test]
    fn targets_respect_trainer_limits_and_are_not_repeated() {
        let limits = ftms_control::Limits {
            power_w: (0, 150),
            resistance: (0, 100),
        };
        let t = &bike_gearing::TEMPLATES[0];
        let shifter = VirtualShifter::new(t.drivetrain().unwrap_or_else(|e| panic!("{e}")), 84.0);
        let mut r = Router::new(limits, Some(shifter));
        let t0 = Instant::now();
        r.trainer_notification(sig(INDOOR_BIKE_DATA), &PEDALLING, t0);
        r.app_write(&SIM);
        while r.shift(Action::ShiftUp).is_some() {}
        // The top gear at 62 rpm uphill wants far more than this trainer allows.
        assert_eq!(watts(r.tick(t0)), 150);
        assert_eq!(r.tick(t0), None, "an unchanged target is not resent");
        assert_eq!(r.shift(Action::ShiftUp), None, "already in the top gear");
    }

    /// Indoor Bike Data with only instantaneous speed and cadence.
    fn cadence_frame(rpm: f64) -> Vec<u8> {
        let [a, b] = ((rpm * 2.0) as u16).to_le_bytes();
        vec![0x04, 0, 0, 0, a, b]
    }

    #[test]
    fn one_odd_cadence_reading_does_not_move_the_load() {
        let mut r = router(true);
        let t0 = Instant::now();
        r.app_write(&SIM);
        let feed = |r: &mut Router, rpm, ms| {
            r.trainer_notification(
                sig(INDOOR_BIKE_DATA),
                &cadence_frame(rpm),
                t0 + Duration::from_millis(ms),
            );
        };
        for (i, rpm) in [90.0, 90.0, 90.0].into_iter().enumerate() {
            feed(&mut r, rpm, 250 * i as u64);
        }
        let steady = watts(r.tick(t0 + Duration::from_millis(500)));
        // A dropout to 0 and a spike between normal readings: the target stays put.
        feed(&mut r, 0.0, 750);
        assert_eq!(r.cadence(t0 + Duration::from_millis(750)), Some(90.0));
        assert_eq!(r.tick(t0 + Duration::from_millis(750)), None);
        feed(&mut r, 90.0, 1000);
        feed(&mut r, 180.0, 1250);
        assert_eq!(r.tick(t0 + Duration::from_millis(1250)), None);
        // Stopping for real still eases off after two readings.
        feed(&mut r, 0.0, 1500);
        feed(&mut r, 0.0, 1750);
        assert!(watts(r.tick(t0 + Duration::from_millis(1750))) < steady);
    }

    #[test]
    fn target_is_sent_again_after_app_commands_and_dropped_writes() {
        let mut r = router(true);
        let t0 = Instant::now();
        r.trainer_notification(sig(INDOOR_BIKE_DATA), &PEDALLING, t0);
        r.app_write(&SIM);
        let target = watts(r.tick(t0));
        assert_eq!(r.tick(t0), None);
        // MyWhoosh's Request Control and Start go to the trainer, then the same target again.
        for command in [
            [ftms_control::REQUEST_CONTROL],
            [ftms_control::START_OR_RESUME],
        ] {
            assert_eq!(r.app_write(&command).to_trainer, vec![command.to_vec()]);
            assert_eq!(r.mode(), Mode::Simulation);
            assert_eq!(watts(r.tick(t0)), target);
            assert_eq!(r.tick(t0), None);
        }
        r.forget_sent();
        assert_eq!(watts(r.tick(t0)), target);
    }

    /// Six seconds of steady pedalling: the real chain is learned.
    fn ride_steadily(r: &mut Router, t0: Instant) -> Instant {
        let mut now = t0;
        for _ in 0..30 {
            r.trainer_notification(sig(INDOOR_BIKE_DATA), &PEDALLING, now);
            now += Duration::from_millis(250);
        }
        now
    }

    fn grade(command: Option<Vec<u8>>) -> i16 {
        let c = command.unwrap_or_else(|| panic!("no road for the trainer"));
        assert_eq!(c[0], ftms_control::SET_SIMULATION, "{c:?}");
        i16::from_le_bytes([c[3], c[4]])
    }

    #[test]
    fn road_feel_sends_a_steeper_road_for_harder_gears() {
        let mut r = router(true);
        let t0 = Instant::now();
        r.app_write(&SIM);
        // Until the real chain is known, power targets keep the gears working.
        r.trainer_notification(sig(INDOOR_BIKE_DATA), &PEDALLING, t0);
        watts(r.tick(t0));
        let now = ride_steadily(&mut r, t0);
        assert!(r.road_feel_active());
        assert!(r.real_chain().is_some_and(|c| (c - 2.448).abs() < 0.01));
        let base = grade(r.tick(now));
        assert_eq!(r.tick(now), None, "an unchanged road is not resent");
        assert!(r.shift(Action::ShiftUp).is_some());
        let harder = grade(r.tick(now));
        assert!(harder > base, "{harder} <= {base}");
        assert!(r.shift(Action::ShiftDown).is_some() && r.shift(Action::ShiftDown).is_some());
        assert!(grade(r.tick(now)) < base);
        // A new road from MyWhoosh goes through the gear as well.
        r.app_write(&[0x11, 0, 0, 0x2c, 0x01, 0x35, 0x33]);
        assert!(grade(r.tick(now)) > base);
        // The trainer's answers to our road are not passed to MyWhoosh.
        assert!(!r.trainer_notification(sig(CONTROL_POINT), &[0x80, 0x11, 0x01], now));
        assert!(!r.trainer_notification(sig(MACHINE_STATUS), &[0x12, 0, 0], now));
        // Stopping pedalling does not matter: the trainer keeps simulating the road.
        let later = now + Duration::from_secs(10);
        assert!(
            r.tick(later)
                .is_none_or(|c| c[0] == ftms_control::SET_SIMULATION),
            "no power target, no easing off"
        );
        assert!(r.road_feel_active());
    }

    #[test]
    fn a_trainer_refusing_the_road_gets_power_targets() {
        let mut r = router(true);
        let t0 = Instant::now();
        r.app_write(&SIM);
        let now = ride_steadily(&mut r, t0);
        grade(r.tick(now));
        assert!(!r.trainer_notification(sig(CONTROL_POINT), &[0x80, 0x11, 0x02], now));
        assert_eq!(r.feel(), ShiftFeel::Power);
        watts(r.tick(now));
        // Power feel never sends a road.
        let mut p = router(true).with_feel(ShiftFeel::Power);
        p.app_write(&SIM);
        let now = ride_steadily(&mut p, t0);
        watts(p.tick(now));
        assert!(!p.road_feel_active());
    }

    #[test]
    fn road_support_is_read_from_the_feature_table() {
        let snapshot = |value: Vec<u8>| Snapshot {
            name: "t".into(),
            limits: ftms_control::Limits::FALLBACK,
            characteristics: vec![MirroredCharacteristic {
                value,
                ..ch(0x1826, 0x2acc, true, false, false, false)
            }],
        };
        // Captured X2 Max features: target settings 0x0000A00C, simulation bit set.
        assert!(simulates_road(&snapshot(vec![
            0x03, 0x40, 0, 0, 0x0c, 0xa0, 0, 0
        ])));
        assert!(!simulates_road(&snapshot(vec![
            0x03, 0x40, 0, 0, 0x0c, 0x80, 0, 0
        ])));
        assert!(simulates_road(&snapshot(vec![])), "unreadable: try it");
    }

    #[test]
    fn load_changes_too_small_to_feel_are_not_resent() {
        let road = |g: i16, cw: u8| {
            let [a, b] = g.to_le_bytes();
            [0x11, 0, 0, a, b, 0x35, cw]
        };
        // 5 % at 3 m/s: about 5 W of 140 W.
        assert!(same_command(&road(500, 51), &road(503, 51), 3.0, 84.0));
        assert!(!same_command(&road(500, 51), &road(530, 51), 3.0, 84.0));
        assert!(!same_command(&road(0, 51), &road(0, 80), 9.0, 84.0));
        // Every shift is sent, even standing still and between near-equal 2x ratios.
        let mut r = router(true).with_real_chain(Some(2.4));
        let t0 = Instant::now();
        r.app_write(&SIM);
        while r.shift(Action::ShiftDown).is_some() {}
        assert!(r.tick(t0).is_some());
        while r.shift(Action::ShiftUp).is_some() {
            assert!(r.tick(t0).is_some(), "gear {:?}", r.gear_info());
        }
        assert!(!same_command(&[0x05, 100, 0], &[0x05, 101, 0], 3.0, 84.0));
        assert!(same_command(&[0x05, 100, 0], &[0x05, 100, 0], 3.0, 84.0));
    }

    #[test]
    fn a_chain_far_from_the_middle_gets_advice() {
        let r = router(true);
        let s = r.shifter().unwrap_or_else(|| panic!("shifter"));
        let gears = s.drivetrain().gears();
        let middle = (gears[0].ratio * gears[gears.len() - 1].ratio).sqrt();
        assert_eq!(chain_hint(s, middle), None);
        let easy = chain_hint(s, middle / 2.0).unwrap_or_default();
        assert!(easy.contains("an easy gear"), "{easy}");
        let hard = chain_hint(s, middle * 2.0).unwrap_or_default();
        assert!(hard.contains("a hard gear"), "{hard}");
        // The suggested gear is one of the rider's own.
        assert!(
            gears
                .iter()
                .any(|g| hard.contains(&format!("{}x{}", g.chainring, g.cog)))
        );
    }

    #[test]
    fn reset_pauses_gears_until_the_road_returns() {
        let mut r = router(true);
        let t0 = Instant::now();
        r.trainer_notification(sig(INDOOR_BIKE_DATA), &PEDALLING, t0);
        r.app_write(&SIM);
        assert!(r.tick(t0).is_some());
        assert_eq!(
            r.app_write(&[ftms_control::RESET]).to_trainer,
            vec![vec![0x01]]
        );
        assert_eq!(r.mode(), Mode::None);
        // Shifting still moves the gear, but nothing drives the trainer until the road is back.
        assert!(r.shift(Action::ShiftUp).is_some());
        assert_eq!(r.tick(t0), None);
        assert!(r.app_write(&SIM).to_trainer.is_empty());
        assert_eq!(r.mode(), Mode::Simulation);
        assert!(r.tick(t0).is_some());
    }

    #[test]
    fn without_gears_nothing_shifts() {
        let mut r = router(false);
        assert_eq!(r.shift(Action::ShiftUp), None);
        assert_eq!(r.gear_info(), None);
        // Trainer replies reach the app untouched when OsmolControl does not shift.
        assert!(r.trainer_notification(sig(CONTROL_POINT), &[0x80, 0x05, 0x01], Instant::now()));
    }

    #[tokio::test]
    #[expect(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "channel plumbing in a test"
    )]
    async fn run_loop_shifts_at_once_relays_both_ways_and_yields_to_erg() {
        let (write_tx, app_writes) = mpsc::channel(8);
        let (trainer_tx, from_trainer) = broadcast::channel(8);
        let (to_app, mut app_rx) = broadcast::channel(8);
        let (to_trainer, mut trainer_rx) = mpsc::channel(8);
        let (action_tx, actions) = broadcast::channel(8);
        let status: StatusTx = std::sync::Arc::new(watch::channel(crate::Status::default()).0);
        let (stop_tx, stop) = watch::channel(false);
        let start_gear = router(true).gear_info().unwrap().number;
        let job = tokio::spawn(run(
            router(true),
            Channels {
                app_writes,
                from_trainer,
                to_app,
                to_trainer,
                actions: Some(actions),
                status: status.clone(),
            },
            stop,
        ));
        let wait = Duration::from_millis(1000);

        // Trainer data reaches MyWhoosh and the status.
        trainer_tx
            .send((sig(INDOOR_BIKE_DATA), PEDALLING.to_vec()))
            .unwrap();
        let relayed = tokio::time::timeout(wait, app_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(relayed, (sig(INDOOR_BIKE_DATA), PEDALLING.to_vec()));
        assert!(status.borrow().telemetry_fresh());

        // The road is acknowledged to MyWhoosh and turned into a power target.
        write_tx
            .send((sig(CONTROL_POINT), SIM.to_vec()))
            .await
            .unwrap();
        let ack = tokio::time::timeout(wait, app_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ack, (sig(CONTROL_POINT), vec![0x80, 0x11, 0x01]));
        let low = watts(tokio::time::timeout(wait, trainer_rx.recv()).await.unwrap());
        assert!(low > 0);

        // A shift goes to the trainer well before the next 250 ms tick.
        action_tx.send((Instant::now(), Action::ShiftUp)).unwrap();
        let shifted = tokio::time::timeout(Duration::from_millis(100), trainer_rx.recv())
            .await
            .expect("the shift must not wait for the ticker");
        assert!(watts(shifted) > low);
        {
            let s = status.borrow();
            assert_eq!(s.gear.map(|g| g.number), Some(start_gear + 1));
            assert!(s.log.iter().any(|l| l.starts_with("Gear ")));
        }

        // A press older than the limit is dropped, not applied late.
        action_tx
            .send((Instant::now() - Duration::from_secs(1), Action::ShiftUp))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(status.borrow().gear.map(|g| g.number), Some(start_gear + 1));

        // ERG from MyWhoosh goes to the trainer as sent; gears no longer drive it.
        write_tx
            .send((sig(CONTROL_POINT), vec![0x05, 0xc8, 0]))
            .await
            .unwrap();
        let erg = tokio::time::timeout(wait, trainer_rx.recv()).await.unwrap();
        assert_eq!(erg, Some(vec![0x05, 0xc8, 0]));
        assert_eq!(status.borrow().control, Mode::Target);
        assert!(
            status
                .borrow()
                .log
                .iter()
                .any(|l| l.contains("gears paused"))
        );
        action_tx.send((Instant::now(), Action::ShiftUp)).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(600), trainer_rx.recv())
                .await
                .is_err(),
            "no gear power target during ERG"
        );

        stop_tx.send(true).unwrap();
        assert!(
            tokio::time::timeout(wait, job)
                .await
                .unwrap()
                .unwrap()
                .is_ok()
        );
    }
}
