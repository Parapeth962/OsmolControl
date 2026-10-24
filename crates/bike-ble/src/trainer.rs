//! The only module that writes to a trainer: FTMS Control Point commands that have already
//! passed `bike_protocol::ftms_control::filter`. Everything else in this crate stays read-only.
use crate::{bounded, find_service, standard_id};
use anyhow::{Context, Result, anyhow, bail};
use bike_core::RetryBudget;
use bike_protocol::ftms_control::{self, CommandQueue, Limits};
use btleplug::{
    api::{CharPropFlags, Characteristic, Peripheral as _, WriteType},
    platform::{Adapter, Peripheral},
};
use futures::StreamExt;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, watch};
use uuid::Uuid;

const FTMS: u16 = 0x1826;
const CYCLING_POWER: u16 = 0x1818;
const CONTROL_POINT: u16 = 0x2ad9;
/// Minimum spacing between control writes (at most 4 per second).
const WRITE_INTERVAL: Duration = Duration::from_millis(250);

/// A characteristic of the real trainer, mirrored to the virtual one.
#[derive(Debug, Clone)]
pub struct MirroredCharacteristic {
    pub service: Uuid,
    pub uuid: Uuid,
    pub readable: bool,
    pub writable: bool,
    pub notify: bool,
    pub indicate: bool,
    pub value: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub name: String,
    pub characteristics: Vec<MirroredCharacteristic>,
    pub limits: Limits,
}

/// Scan once for any device advertising Fitness Machine (FTMS); strongest signal wins.
pub async fn find_trainer(a: &Adapter, seconds: u64) -> Result<Option<(Peripheral, String)>> {
    find_service(a, seconds, sig(FTMS), false).await
}

pub(crate) fn sig(short: u16) -> Uuid {
    Uuid::from_u128((u128::from(short) << 96) | 0x0000_0000_0000_1000_8000_0080_5f9b_34fb)
}

/// Connect and read the FTMS and Cycling Power tables so they can be mirrored truthfully.
pub async fn snapshot(p: &Peripheral, name: String) -> Result<Snapshot> {
    bounded("connect", p.connect()).await?;
    bounded("discover services", p.discover_services()).await?;
    let mut characteristics = vec![];
    for c in p.characteristics() {
        if !matches!(standard_id(c.service_uuid), Some(FTMS | CYCLING_POWER)) {
            continue;
        }
        let readable = c.properties.contains(CharPropFlags::READ);
        let value = if readable {
            bounded("read", p.read(&c)).await.unwrap_or_default()
        } else {
            vec![]
        };
        characteristics.push(MirroredCharacteristic {
            service: c.service_uuid,
            uuid: c.uuid,
            readable,
            writable: c
                .properties
                .intersects(CharPropFlags::WRITE | CharPropFlags::WRITE_WITHOUT_RESPONSE),
            notify: c.properties.contains(CharPropFlags::NOTIFY),
            indicate: c.properties.contains(CharPropFlags::INDICATE),
            value,
        });
    }
    if !characteristics.iter().any(|c| c.uuid == sig(CONTROL_POINT)) {
        bail!("{name} has no FTMS Control Point; it cannot be controlled");
    }
    let value = |id| {
        characteristics
            .iter()
            .find(|c| c.uuid == sig(id))
            .map_or(&[][..], |c| c.value.as_slice())
    };
    let limits = Limits::from_ranges(value(0x2ad8), value(0x2ad6)).unwrap_or_else(|| {
        tracing::warn!("trainer ranges unreadable; using conservative limits");
        Limits::FALLBACK
    });
    Ok(Snapshot {
        name,
        characteristics,
        limits,
    })
}

/// Remembers what the app last asked for, so control survives a trainer reconnect.
#[derive(Debug, Default)]
pub struct ControlState {
    has_control: bool,
    last_target: Option<Vec<u8>>,
}

impl ControlState {
    pub fn record(&mut self, command: &[u8]) {
        match command.first().copied() {
            Some(ftms_control::REQUEST_CONTROL) => self.has_control = true,
            Some(ftms_control::RESET) => *self = Self::default(),
            Some(
                ftms_control::SET_TARGET_POWER
                | ftms_control::SET_TARGET_RESISTANCE
                | ftms_control::SET_SIMULATION,
            ) => {
                self.last_target = Some(command.to_vec());
            }
            _ => {}
        }
    }
    /// Commands that restore the app's state on a fresh connection.
    pub fn replay(&self) -> Vec<Vec<u8>> {
        if !self.has_control {
            return vec![];
        }
        std::iter::once(vec![ftms_control::REQUEST_CONTROL])
            .chain(self.last_target.clone())
            .collect()
    }
}

/// Relay notifications up and filtered control commands down, reconnecting with bounded retries.
pub async fn run(
    p: Peripheral,
    notifications: broadcast::Sender<(Uuid, Vec<u8>)>,
    mut commands: mpsc::Receiver<Vec<u8>>,
    mut stop: watch::Receiver<bool>,
    connected: watch::Sender<bool>,
) -> Result<()> {
    let mut retry = RetryBudget::default();
    let mut control = ControlState::default();
    loop {
        let started = tokio::time::Instant::now();
        let result = session(
            &p,
            &notifications,
            &mut commands,
            &mut control,
            &mut stop,
            &connected,
        )
        .await;
        let _ = connected.send(false);
        if *stop.borrow() {
            // Hand the trainer back in its default state; best effort.
            if control.has_control
                && let Some(cp) = p
                    .characteristics()
                    .into_iter()
                    .find(|c| c.uuid == sig(CONTROL_POINT))
            {
                let _ = bounded(
                    "reset",
                    p.write(&cp, &[ftms_control::RESET], WriteType::WithResponse),
                )
                .await;
            }
            let _ = bounded("disconnect", crate::release(&p)).await;
            return result;
        }
        let _ = bounded("disconnect", crate::release(&p)).await;
        if started.elapsed() > Duration::from_secs(30) {
            retry.stable();
        }
        let error = result
            .err()
            .unwrap_or_else(|| anyhow!("trainer session ended"));
        tracing::warn!(error = %format!("{error:#}"), "trainer disconnected; reconnecting");
        let Some(delay) = retry.next_delay() else {
            return Err(error.context("trainer reconnect budget exhausted"));
        };
        tokio::select! { _ = tokio::time::sleep(delay) => {}, _ = stop.changed() => {} }
        if *stop.borrow() {
            return Ok(());
        }
    }
}

async fn session(
    p: &Peripheral,
    notifications: &broadcast::Sender<(Uuid, Vec<u8>)>,
    commands: &mut mpsc::Receiver<Vec<u8>>,
    control: &mut ControlState,
    stop: &mut watch::Receiver<bool>,
    connected: &watch::Sender<bool>,
) -> Result<()> {
    if !bounded("connection state", p.is_connected()).await? {
        bounded("connect", p.connect()).await?;
        bounded("discover services", p.discover_services()).await?;
    }
    let chars: Vec<Characteristic> = p.characteristics().into_iter().collect();
    let control_point = chars
        .iter()
        .find(|c| c.uuid == sig(CONTROL_POINT))
        .cloned()
        .context("FTMS Control Point missing")?;
    let mut stream = bounded("notification stream", p.notifications()).await?;
    for c in &chars {
        let relayed = matches!(standard_id(c.service_uuid), Some(FTMS | CYCLING_POWER));
        if relayed
            && c.properties
                .intersects(CharPropFlags::NOTIFY | CharPropFlags::INDICATE)
        {
            bounded("subscribe", p.subscribe(c)).await?;
        }
    }
    let mut queue = CommandQueue::default();
    for command in control.replay() {
        queue.push(command);
    }
    tracing::info!("trainer connected");
    let _ = connected.send(true);
    let mut pace = tokio::time::interval(WRITE_INTERVAL);
    let mut health = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = stop.changed() => return Ok(()),
            n = stream.next() => {
                let n = n.ok_or_else(|| anyhow!("notification stream ended"))?;
                let _ = notifications.send((n.uuid, n.value));
            }
            c = commands.recv() => {
                let Some(c) = c else { return Ok(()) };
                if !queue.push(c) {
                    tracing::warn!("trainer command queue full; command dropped");
                }
            }
            _ = pace.tick(), if !queue.is_empty() => {
                if let Some(c) = queue.pop() {
                    bounded("control write", p.write(&control_point, &c, WriteType::WithResponse)).await?;
                    control.record(&c);
                }
            }
            _ = health.tick() => {
                if !bounded("connection health", p.is_connected()).await? {
                    bail!("trainer disconnected");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_restores_control_and_latest_target_only() {
        let mut s = ControlState::default();
        s.record(&[0x11, 0, 0, 1, 0, 0, 0]);
        assert!(s.replay().is_empty(), "never had control");
        s.record(&[0x00]);
        s.record(&[0x05, 100, 0]);
        s.record(&[0x11, 0, 0, 2, 0, 0, 0]);
        assert_eq!(s.replay(), vec![vec![0x00], vec![0x11, 0, 0, 2, 0, 0, 0]]);
        s.record(&[0x01]);
        assert!(s.replay().is_empty(), "reset releases control");
    }
}
