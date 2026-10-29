//! Virtual trainer: mirror the real trainer over DirCon, relay filtered control commands and
//! optionally replace the app's road simulation with gear-aware power targets.
use anyhow::Result;
use bike_ble::trainer::Snapshot;
use bike_dircon::{Characteristic, Gatt, Service, gatt, sig};
use bike_gearing::{Gear, Simulation, VirtualShifter};
use bike_protocol::Action;
use bike_protocol::ftms_control::{self, Verdict};
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

use crate::{Control as Mode, GearInfo, StatusTx};

/// Cadence older than this is treated as unknown (the rider stopped or the trainer went quiet).
const CADENCE_MAX_AGE: Duration = crate::TELEMETRY_MAX_AGE;

/// Pure routing between the app, the trainer and the virtual drivetrain.
pub struct Router {
    limits: ftms_control::Limits,
    shifter: Option<VirtualShifter>,
    mode: Mode,
    cadence: Option<(f64, Instant)>,
    last_tick: Option<Instant>,
    last_sent_w: Option<i16>,
}

/// Outcome of an app write: commands for the trainer and notifications for the app.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Routed {
    pub to_trainer: Vec<Vec<u8>>,
    pub to_app: Vec<(Uuid, Vec<u8>)>,
}

impl Router {
    pub fn new(limits: ftms_control::Limits, shifter: Option<VirtualShifter>) -> Self {
        Self {
            limits,
            shifter,
            mode: Mode::None,
            cadence: None,
            last_tick: None,
            last_sent_w: None,
        }
    }

    pub fn shifter(&self) -> Option<&VirtualShifter> {
        self.shifter.as_ref()
    }

    pub fn mode(&self) -> Mode {
        self.mode
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
            // Consumed here: the trainer receives gear-aware power targets instead.
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
            _ => {}
        }
        tracing::debug!(command = %bike_protocol::hex(&command), "control command relayed");
        Routed {
            to_trainer: vec![command],
            ..Routed::default()
        }
    }

    fn enter(&mut self, mode: Mode) {
        self.mode = mode;
        self.last_sent_w = None;
        if let Some(s) = &mut self.shifter {
            s.clear_simulation();
        }
    }

    /// Track cadence; hide trainer replies to OsmolControl's own power targets from the app.
    pub fn trainer_notification(&mut self, uuid: Uuid, value: &[u8], now: Instant) -> bool {
        if uuid == sig(INDOOR_BIKE_DATA)
            && let Ok(v) = bike_protocol::indoor_bike(value)
            && let Some(rpm) = v.get("cadence_rpm").and_then(serde_json::Value::as_f64)
        {
            self.cadence = Some((rpm, now));
        }
        let injected = self.mode == Mode::Simulation && self.shifter.is_some();
        let own_reply = uuid == sig(CONTROL_POINT)
            && value.starts_with(&[ftms_control::RESPONSE, ftms_control::SET_TARGET_POWER]);
        // FTMS Machine Status 0x08 = "target power changed".
        let own_status = uuid == sig(MACHINE_STATUS) && value.first() == Some(&0x08);
        !(injected && (own_reply || own_status))
    }

    /// Periodic power target in virtual-shifting mode.
    pub fn tick(&mut self, now: Instant) -> Option<Vec<u8>> {
        let dt = self
            .last_tick
            .map_or(0.0, |t| now.duration_since(t).as_secs_f64());
        self.last_tick = Some(now);
        if self.mode != Mode::Simulation {
            return None;
        }
        let cadence = self
            .cadence
            .filter(|(_, at)| now.duration_since(*at) <= CADENCE_MAX_AGE)
            .map(|(rpm, _)| rpm);
        let target = self.shifter.as_mut()?.update(cadence, dt)?;
        // Bounded by the clamp and the trainer range below; rounding to whole watts.
        let watts = target.round().clamp(0.0, f64::from(i16::MAX)) as i16;
        if self.last_sent_w == Some(watts) {
            return None;
        }
        self.last_sent_w = Some(watts);
        let [a, b] = watts.to_le_bytes();
        match ftms_control::filter(&[ftms_control::SET_TARGET_POWER, a, b], &self.limits) {
            Verdict::Forward(command) => Some(command),
            Verdict::Reject(_) => None,
        }
    }

    /// Returns the new gear if it changed.
    pub fn shift(&mut self, action: Action) -> Option<Gear> {
        let shifter = self.shifter.as_mut()?;
        let harder = match action {
            Action::ShiftUp => true,
            Action::ShiftDown => false,
        };
        shifter.shift(harder).then(|| shifter.gear())
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
                for c in routed.to_trainer {
                    send(&ch.to_trainer, c);
                }
                for n in routed.to_app {
                    let _ = ch.to_app.send(n);
                }
                publish(&router, &ch.status, grade, target);
            }
            n = ch.from_trainer.recv() => match n {
                Ok((uuid, value)) => {
                    if uuid == sig(INDOOR_BIKE_DATA) {
                        telemetry(&ch.status, &value);
                    }
                    if router.trainer_notification(uuid, &value, Instant::now()) {
                        let _ = ch.to_app.send((uuid, value));
                    }
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
            _ = ticker.tick() => power_target(&mut router, &ch),
        }
    }
}

fn send(tx: &mpsc::Sender<Vec<u8>>, command: Vec<u8>) {
    if tx.try_send(command).is_err() {
        tracing::warn!("trainer busy; control command dropped");
    }
}

/// Send the router's next power target to the trainer, if any.
fn power_target(router: &mut Router, ch: &Channels) {
    let Some(c) = router.tick(Instant::now()) else {
        return;
    };
    let target = i16::from_le_bytes([c[1], c[2]]);
    {
        let s = ch.status.borrow();
        tracing::debug!(
            target,
            power = ?s.power_w,
            cadence = ?s.cadence_rpm,
            grade = ?s.grade_percent,
            gear = ?s.gear.as_ref().map(|g| g.number),
            "power target"
        );
    }
    send(&ch.to_trainer, c);
    publish(router, &ch.status, None, Some(target));
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
