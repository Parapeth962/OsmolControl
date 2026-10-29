//! Cross-platform BLE central adapter. Read-only except the `trainer` module.
use anyhow::{Context, Result, anyhow, bail};
use bike_core::{PacketMap, RetryBudget};
use bike_protocol::Action;
use btleplug::{
    api::{
        Central, CharPropFlags, Manager as _, Peripheral as _, PeripheralProperties, ScanFilter,
    },
    platform::{Adapter, Manager},
};
use futures::StreamExt;
use serde_json::{Value, json};
use std::{
    future::Future,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{broadcast, mpsc, watch};
use uuid::Uuid;
pub mod trainer;
pub use btleplug::platform::Peripheral;

type NotificationStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = btleplug::api::ValueNotification> + Send>>;
// A deliberately read-only seam: mock tests cannot accidentally send resistance writes.
trait Link: Send + Sync {
    fn label(&self) -> String;
    fn chars(&self) -> Vec<btleplug::api::Characteristic>;
    fn is_up(&self) -> impl Future<Output = btleplug::Result<bool>> + Send;
    fn open(&self) -> impl Future<Output = btleplug::Result<()>> + Send;
    fn close(&self) -> impl Future<Output = btleplug::Result<()>> + Send;
    fn discover(&self) -> impl Future<Output = btleplug::Result<()>> + Send;
    fn listen<'a>(
        &'a self,
        c: &'a btleplug::api::Characteristic,
    ) -> impl Future<Output = btleplug::Result<()>> + Send + 'a;
    fn stream(&self) -> impl Future<Output = btleplug::Result<NotificationStream>> + Send;
}
impl Link for Peripheral {
    fn label(&self) -> String {
        self.id().to_string()
    }
    fn chars(&self) -> Vec<btleplug::api::Characteristic> {
        self.characteristics().into_iter().collect()
    }
    fn is_up(&self) -> impl Future<Output = btleplug::Result<bool>> + Send {
        self.is_connected()
    }
    fn open(&self) -> impl Future<Output = btleplug::Result<()>> + Send {
        self.connect()
    }
    fn close(&self) -> impl Future<Output = btleplug::Result<()>> + Send {
        release(self)
    }
    fn discover(&self) -> impl Future<Output = btleplug::Result<()>> + Send {
        self.discover_services()
    }
    fn listen<'a>(
        &'a self,
        c: &'a btleplug::api::Characteristic,
    ) -> impl Future<Output = btleplug::Result<()>> + Send + 'a {
        self.subscribe(c)
    }
    fn stream(&self) -> impl Future<Output = btleplug::Result<NotificationStream>> + Send {
        self.notifications()
    }
}
#[derive(Default)]
struct SessionSignals {
    connected_at: Option<tokio::time::Instant>,
    ready: Option<watch::Sender<bool>>,
}
const IO_TIMEOUT: Duration = Duration::from_secs(12);
/// How often a live link is checked for a silent disconnect.
pub(crate) const HEALTH_INTERVAL: Duration = Duration::from_secs(2);
/// A device sending measurements is reported stale after this long without one.
const STALE_AFTER: Duration = Duration::from_secs(5);
/// A connection that lasted this long resets the reconnect budget.
pub(crate) const STABLE_AFTER: Duration = Duration::from_secs(30);
/// Standard measurements: Indoor Bike Data, Cycling Power, CSC and Heart Rate.
const MEASUREMENTS: [u16; 4] = [0x2ad2, 0x2a63, 0x2a5b, 0x2a37];
/// Battery Level and FTMS Machine Status.
const BATTERY: u16 = 0x2a19;
const MACHINE_STATUS: u16 = 0x2ada;
pub type Events = mpsc::Sender<Value>;
pub type Actions = broadcast::Sender<(Instant, Action)>;
async fn bounded<T>(label: &str, f: impl Future<Output = btleplug::Result<T>>) -> Result<T> {
    tokio::time::timeout(IO_TIMEOUT, f)
        .await
        .with_context(|| format!("{label}: timed out"))?
        .with_context(|| label.to_owned())
}
pub fn timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
fn emit(tx: &Events, event: Value) {
    if let Err(error) = tx.try_send(event) {
        tracing::warn!(%error, "diagnostic event dropped; consumer too slow or closed");
    }
}
fn notifies(c: &btleplug::api::Characteristic) -> bool {
    c.properties
        .intersects(CharPropFlags::NOTIFY | CharPropFlags::INDICATE)
}
/// Stop notifications, then disconnect. BlueZ keeps a notification registered for as long as the
/// process's D-Bus connection lives, so a later session in the same process (the GUI's next
/// ride) would otherwise receive every notification twice. Unsubscribe errors are ignored:
/// not every characteristic was subscribed, and a lost link cannot be unsubscribed.
pub(crate) async fn release(p: &Peripheral) -> btleplug::Result<()> {
    for c in p.characteristics().iter().filter(|c| notifies(c)) {
        let _ = p.unsubscribe(c).await;
    }
    p.disconnect().await
}
pub async fn adapter(index: usize) -> Result<Adapter> {
    let manager = bounded("initialize Bluetooth", Manager::new()).await?;
    let adapters = bounded("list adapters", manager.adapters()).await?;
    adapters.into_iter().nth(index).ok_or_else(|| {
        anyhow!(
            "Bluetooth adapter {index} unavailable; check Bluetooth power, BlueZ and permissions"
        )
    })
}
pub async fn adapters() -> Result<Vec<String>> {
    let m = bounded("initialize Bluetooth", Manager::new()).await?;
    let mut out = vec![];
    for a in bounded("list adapters", m.adapters()).await? {
        out.push(bounded("adapter info", a.adapter_info()).await?);
    }
    Ok(out)
}
/// Scan for `seconds`, then return every known device with its advertisement.
async fn scan_devices(
    a: &Adapter,
    seconds: u64,
) -> Result<Vec<(Peripheral, PeripheralProperties)>> {
    bounded("start scan", a.start_scan(ScanFilter::default())).await?;
    let cancelled = tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(seconds)) => false,
        _ = tokio::signal::ctrl_c() => true,
    };
    bounded("stop scan", a.stop_scan()).await?;
    if cancelled {
        bail!("scan cancelled");
    }
    let mut found = vec![];
    for p in bounded("list peripherals", a.peripherals()).await? {
        if let Some(props) = bounded("read advertisement", p.properties()).await? {
            found.push((p, props));
        }
    }
    Ok(found)
}
pub async fn scan(a: &Adapter, seconds: u64) -> Result<Vec<Value>> {
    Ok(scan_devices(a, seconds)
        .await?
        .into_iter()
        .map(|(p, v)| {
            json!({
                "id": p.id().to_string(),
                "name": v.local_name,
                "rssi": v.rssi,
                "services": v.services,
                "manufacturer_data": v.manufacturer_data,
                "service_data": v.service_data,
            })
        })
        .collect())
}
pub async fn find(a: &Adapter, selector: &str, seconds: u64) -> Result<Peripheral> {
    let mut matches = vec![];
    for (p, props) in scan_devices(a, seconds).await? {
        let name = props.local_name;
        if p.id().to_string().eq_ignore_ascii_case(selector) || name.as_deref() == Some(selector) {
            matches.push(p);
        }
    }
    if matches.len() != 1 {
        bail!(
            "selector {selector:?} matched {} devices; use an exact ID from scan",
            matches.len()
        );
    }
    matches.pop().ok_or_else(|| anyhow!("device disappeared"))
}
/// One bounded scan for devices advertising `service`; strongest signal wins. With `on_air`,
/// devices BlueZ only remembers from earlier (no signal in this scan) are skipped.
pub(crate) async fn find_service(
    a: &Adapter,
    seconds: u64,
    service: Uuid,
    on_air: bool,
) -> Result<Option<(Peripheral, String)>> {
    let mut best: Option<(i16, Peripheral, String)> = None;
    for (p, props) in scan_devices(a, seconds).await? {
        if !props.services.contains(&service) || (on_air && props.rssi.is_none()) {
            continue;
        }
        let rssi = props.rssi.unwrap_or(i16::MIN);
        let name = props.local_name.unwrap_or_else(|| p.id().to_string());
        if best.as_ref().is_none_or(|(r, _, _)| rssi > *r) {
            best = Some((rssi, p, name));
        }
    }
    Ok(best.map(|(_, p, n)| (p, n)))
}
/// One bounded scan for a heart rate sensor (Heart Rate service) that is on air now.
pub async fn find_heart_rate(a: &Adapter, seconds: u64) -> Result<Option<(Peripheral, String)>> {
    find_service(a, seconds, trainer::sig(0x180d), true).await
}
/// One bounded scan for any of `profiles`; `None` if nothing known is advertising.
pub async fn find_known_controller(
    a: &Adapter,
    seconds: u64,
    profiles: &[bike_core::ControllerProfile],
) -> Result<Option<(Peripheral, bike_core::ControllerProfile, String)>> {
    for (p, props) in scan_devices(a, seconds).await? {
        let services: Vec<String> = props.services.iter().map(Uuid::to_string).collect();
        let name = props.local_name.unwrap_or_default();
        if let Some(profile) = bike_core::controller_profile(profiles, &name, &services) {
            let name = if name.is_empty() {
                p.id().to_string()
            } else {
                name
            };
            return Ok(Some((p, profile.clone(), name)));
        }
    }
    Ok(None)
}
/// A device seen in a scan, for picking a controller to learn.
#[derive(Clone)]
pub struct Nearby {
    pub peripheral: Peripheral,
    pub name: String,
    pub services: Vec<String>,
    pub rssi: Option<i16>,
}
/// One bounded scan; named devices only, strongest signal first.
pub async fn nearby(a: &Adapter, seconds: u64) -> Result<Vec<Nearby>> {
    let mut found = vec![];
    for (p, props) in scan_devices(a, seconds).await? {
        if let Some(name) = props.local_name.filter(|n| !n.trim().is_empty()) {
            found.push(Nearby {
                peripheral: p,
                name,
                services: props.services.iter().map(Uuid::to_string).collect(),
                rssi: props.rssi,
            });
        }
    }
    found.sort_by_key(|n| std::cmp::Reverse(n.rssi.unwrap_or(i16::MIN)));
    Ok(found)
}
/// Subscribe to every notifying characteristic and forward (characteristic, bytes) until
/// `stop`, for learning an unknown controller; `ready` fires once subscribed. Read-only like
/// the monitor.
pub async fn listen_all(
    p: &Peripheral,
    tx: mpsc::UnboundedSender<(String, Vec<u8>)>,
    ready: tokio::sync::oneshot::Sender<()>,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let result = async {
        connect(p).await?;
        let mut notifications = bounded("notification stream", p.notifications()).await?;
        let mut count = 0;
        for c in p.characteristics().iter().filter(|c| notifies(c)) {
            bounded("subscribe", p.subscribe(c)).await?;
            count += 1;
        }
        if count == 0 {
            bail!("this device sends no notifications, so it cannot be a supported controller");
        }
        let _ = ready.send(());
        let mut health = tokio::time::interval(HEALTH_INTERVAL);
        loop {
            tokio::select! {
                _ = stop.changed() => return Ok(()),
                n = notifications.next() => {
                    let n = n.ok_or_else(|| anyhow!("notification stream ended"))?;
                    if tx.send((n.uuid.to_string(), n.value)).is_err() {
                        return Ok(());
                    }
                }
                _ = health.tick() => {
                    if !bounded("connection health", p.is_connected()).await? {
                        bail!("device disconnected");
                    }
                }
            }
        }
    }
    .await;
    let _ = bounded("disconnect", release(p)).await;
    result
}
fn standard_id(uuid: Uuid) -> Option<u16> {
    let s = uuid.to_string();
    if s.starts_with("0000") && s.ends_with("-0000-1000-8000-00805f9b34fb") {
        u16::from_str_radix(s.get(4..8)?, 16).ok()
    } else {
        None
    }
}
fn decode(uuid: Uuid, bytes: &[u8]) -> Value {
    match standard_id(uuid) {
        Some(id) => match bike_protocol::characteristic(id, bytes) {
            Ok(v) => v,
            Err(e) => json!({"decode_error": e.to_string(), "raw_hex": bike_protocol::hex(bytes)}),
        },
        None => json!({"raw_hex": bike_protocol::hex(bytes)}),
    }
}
async fn connect(p: &impl Link) -> Result<()> {
    if !bounded("connection state", p.is_up()).await? {
        bounded("connect", p.open()).await?;
    }
    bounded("discover services", p.discover()).await
}
pub async fn inspect(p: &Peripheral) -> Result<Value> {
    let result = async {
        connect(p).await?;
        let mut services = vec![];
        for service in p.services() {
            let mut chars = vec![];
            for c in service.characteristics {
                let descriptors: Vec<String> =
                    c.descriptors.iter().map(|d| d.uuid.to_string()).collect();
                let mut row = json!({
                    "uuid": c.uuid,
                    "properties": format!("{:?}", c.properties),
                    "descriptors": descriptors,
                });
                if c.properties.contains(CharPropFlags::READ) {
                    match bounded("read characteristic", p.read(&c)).await {
                        Ok(bytes) => {
                            row["raw_hex"] = json!(bike_protocol::hex(&bytes));
                            row["value"] = decode(c.uuid, &bytes);
                        }
                        Err(e) => row["error"] = json!(format!("{e:#}")),
                    }
                }
                chars.push(row);
            }
            services.push(json!({
                "uuid": service.uuid,
                "primary": service.primary,
                "characteristics": chars,
            }));
        }
        Ok::<_, anyhow::Error>(json!({
            "id": p.id().to_string(),
            "timestamp_ms": timestamp_ms(),
            "services": services,
        }))
    }
    .await;
    let cleanup = bounded("disconnect", p.disconnect()).await;
    let inspected = result?;
    cleanup?;
    Ok(inspected)
}
/// Mapped controllers skip Battery notifications: with 2A19 also subscribed, the V100 delivered
/// only some FEA1 presses (docs/devices/v100.md), and battery is not forwarded to MyWhoosh.
fn wants_subscription(c: &btleplug::api::Characteristic, custom: Option<Uuid>) -> bool {
    let standard = match standard_id(c.uuid) {
        Some(BATTERY) => custom.is_none(),
        Some(id) => id == MACHINE_STATUS || MEASUREMENTS.contains(&id),
        None => false,
    };
    (standard || custom == Some(c.uuid)) && notifies(c)
}
async fn session(
    p: &impl Link,
    tx: &Events,
    mapping: Option<&PacketMap>,
    actions: Option<&Actions>,
    stop: &mut watch::Receiver<bool>,
    signals: &mut SessionSignals,
) -> Result<()> {
    connect(p).await?;
    // Button-state decoders remember pressed buttons; start each connection released.
    let mut decoder = mapping.cloned();
    let custom = mapping
        .map(|m| Uuid::parse_str(&m.characteristic))
        .transpose()
        .context("controller characteristic UUID")?;
    let mut notifications = bounded("notification stream", p.stream()).await?;
    let mut count = 0;
    let mut has_measurements = false;
    let mut custom_found = false;
    for c in p.chars() {
        if wants_subscription(&c, custom) {
            bounded("subscribe", p.listen(&c)).await?;
            count += 1;
            has_measurements |= standard_id(c.uuid).is_some_and(|id| MEASUREMENTS.contains(&id));
            custom_found |= custom == Some(c.uuid);
        }
    }
    if custom.is_some() && !custom_found {
        bail!("configured controller characteristic not found or not notifiable");
    }
    if count == 0 {
        bail!(
            "no supported measurement subscriptions; inspect device and supply a verified mapping for vendor events"
        );
    }
    signals.connected_at = Some(tokio::time::Instant::now());
    if let Some(ready) = &signals.ready {
        let _ = ready.send(true);
    }
    let id = p.label();
    let event = |name: &str| json!({"device": id, "event": name, "timestamp_ms": timestamp_ms()});
    emit(tx, event("connected"));
    let mut health = tokio::time::interval(HEALTH_INTERVAL);
    let mut last = tokio::time::Instant::now();
    let mut stale = false;
    loop {
        tokio::select! {
            _ = stop.changed() => return Ok(()),
            n = notifications.next() => {
                let n = n.ok_or_else(|| anyhow!("notification stream ended"))?;
                last = tokio::time::Instant::now();
                stale = false;
                let mut measurement = event("measurement");
                measurement["characteristic"] = json!(n.uuid);
                measurement["raw_hex"] = json!(bike_protocol::hex(&n.value));
                measurement["value"] = decode(n.uuid, &n.value);
                emit(tx, measurement);
                if custom == Some(n.uuid)
                    && let Some(action) = decoder.as_mut().and_then(|m| m.decode(&n.value))
                {
                    tracing::debug!(device = %id, ?action, raw = %bike_protocol::hex(&n.value), "controller action");
                    let mut pressed = event("controller_action");
                    pressed["action"] = json!(action);
                    emit(tx, pressed);
                    if let Some(sender) = actions {
                        let _ = sender.send((Instant::now(), action));
                    }
                }
            }
            _ = health.tick() => {
                if !bounded("connection health", p.is_up()).await? {
                    bail!("device disconnected");
                }
                if has_measurements && !stale && last.elapsed() > STALE_AFTER {
                    stale = true;
                    emit(tx, event("stale"));
                }
            }
        }
    }
}
/// Independent bounded retries per device; disconnect on cancellation/error.
async fn monitor_link(
    p: impl Link,
    tx: Events,
    mapping: Option<PacketMap>,
    actions: Option<Actions>,
    mut stop: watch::Receiver<bool>,
    ready: Option<watch::Sender<bool>>,
) -> Result<()> {
    let mut retry = RetryBudget::default();
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        let mut signals = SessionSignals {
            connected_at: None,
            ready: ready.clone(),
        };
        let mut cancel = stop.clone();
        let cancelled = async {
            while !*cancel.borrow() {
                if cancel.changed().await.is_err() {
                    break;
                }
            }
        };
        let result = tokio::select! {
            r = session(&p, &tx, mapping.as_ref(), actions.as_ref(), &mut stop, &mut signals) => r,
            () = cancelled => Ok(()),
        };
        if let Some(ready) = &ready {
            let _ = ready.send(false);
        }
        let _ = bounded("disconnect", p.close()).await;
        if *stop.borrow() {
            return Ok(());
        }
        if signals
            .connected_at
            .is_some_and(|at| at.elapsed() > STABLE_AFTER)
        {
            retry.stable();
        }
        let error = result.err().unwrap_or_else(|| anyhow!("session ended"));
        emit(
            &tx,
            json!({
                "device": p.label(),
                "event": "disconnected",
                "error": format!("{error:#}"),
                "timestamp_ms": timestamp_ms(),
            }),
        );
        let Some(delay) = retry.next_delay() else {
            return Err(error.context("reconnect budget exhausted"));
        };
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            _ = stop.changed() => return Ok(()),
        }
    }
}

/// Monitor a platform peripheral; optional readiness tracks a mapped controller.
pub async fn monitor_with_readiness(
    p: Peripheral,
    tx: Events,
    mapping: Option<PacketMap>,
    actions: Option<Actions>,
    stop: watch::Receiver<bool>,
    ready: Option<watch::Sender<bool>>,
) -> Result<()> {
    monitor_link(p, tx, mapping, actions, stop, ready).await
}

/// Read-only monitoring without publishing controller readiness to another component.
pub async fn monitor(
    p: Peripheral,
    tx: Events,
    mapping: Option<PacketMap>,
    actions: Option<Actions>,
    stop: watch::Receiver<bool>,
) -> Result<()> {
    monitor_with_readiness(p, tx, mapping, actions, stop, None).await
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "synthetic transport setup and assertions"
)]
mod tests {
    use super::*;
    use btleplug::api::{Characteristic, ValueNotification};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    #[derive(Default)]
    struct State {
        up: AtomicBool,
        opens: AtomicUsize,
        closes: AtomicUsize,
        subscriptions: AtomicUsize,
        fail: AtomicBool,
        hang: AtomicBool,
    }
    #[derive(Clone, Default)]
    struct Mock(Arc<State>);
    fn characteristic() -> Characteristic {
        Characteristic {
            uuid: Uuid::from_u128(0x00002ad2_0000_1000_8000_00805f9b34fb),
            service_uuid: Uuid::from_u128(0x00001826_0000_1000_8000_00805f9b34fb),
            properties: CharPropFlags::NOTIFY,
            descriptors: Default::default(),
        }
    }
    impl Link for Mock {
        fn label(&self) -> String {
            "synthetic-trainer".into()
        }
        fn chars(&self) -> Vec<Characteristic> {
            vec![characteristic()]
        }
        async fn is_up(&self) -> btleplug::Result<bool> {
            Ok(self.0.up.load(Ordering::SeqCst))
        }
        async fn open(&self) -> btleplug::Result<()> {
            self.0.opens.fetch_add(1, Ordering::SeqCst);
            if self.0.hang.load(Ordering::SeqCst) {
                futures::future::pending::<()>().await;
            }
            if self.0.fail.load(Ordering::SeqCst) {
                return Err(btleplug::Error::NotConnected);
            }
            self.0.up.store(true, Ordering::SeqCst);
            Ok(())
        }
        async fn close(&self) -> btleplug::Result<()> {
            self.0.closes.fetch_add(1, Ordering::SeqCst);
            self.0.up.store(false, Ordering::SeqCst);
            Ok(())
        }
        async fn discover(&self) -> btleplug::Result<()> {
            Ok(())
        }
        async fn listen<'a>(&'a self, _: &'a Characteristic) -> btleplug::Result<()> {
            self.0.subscriptions.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn stream(&self) -> btleplug::Result<NotificationStream> {
            Ok(Box::pin(
                futures::stream::iter(vec![ValueNotification {
                    uuid: characteristic().uuid,
                    value: vec![0, 0, 0xc4, 9],
                }])
                .chain(futures::stream::pending()),
            ))
        }
    }
    #[test]
    fn mapped_controller_skips_battery() {
        let notify = |id: u128| Characteristic {
            uuid: Uuid::from_u128(id),
            service_uuid: Uuid::nil(),
            properties: CharPropFlags::NOTIFY,
            descriptors: Default::default(),
        };
        let battery = notify(0x00002a19_0000_1000_8000_00805f9b34fb);
        let buttons = notify(0x0000fea1_0000_1000_8000_00805f9b34fb);
        assert!(wants_subscription(&battery, None));
        assert!(!wants_subscription(&buttons, None));
        assert!(!wants_subscription(&battery, Some(buttons.uuid)));
        assert!(wants_subscription(&buttons, Some(buttons.uuid)));
        assert!(wants_subscription(&characteristic(), Some(buttons.uuid)));
    }
    #[tokio::test(start_paused = true)]
    async fn failures_exhaust_budget_and_disconnect() {
        let p = Mock::default();
        p.0.fail.store(true, Ordering::SeqCst);
        let (tx, _rx) = mpsc::channel(32);
        let (_stop, rx) = watch::channel(false);
        let result = monitor_link(p.clone(), tx, None, None, rx, None).await;
        assert!(result.is_err());
        assert_eq!(p.0.opens.load(Ordering::SeqCst), 6);
        assert_eq!(p.0.closes.load(Ordering::SeqCst), 6);
    }
    #[tokio::test(start_paused = true)]
    async fn cancellation_interrupts_hung_connection() {
        let p = Mock::default();
        p.0.hang.store(true, Ordering::SeqCst);
        let (tx, _rx) = mpsc::channel(32);
        let (stop, rx) = watch::channel(false);
        let job = tokio::spawn(monitor_link(p.clone(), tx, None, None, rx, None));
        tokio::task::yield_now().await;
        stop.send(true).unwrap();
        job.await.unwrap().unwrap();
        assert_eq!(p.0.closes.load(Ordering::SeqCst), 1);
    }
    #[tokio::test(start_paused = true)]
    async fn disconnect_reconnect_resubscribes_and_readiness_clears() {
        let p = Mock::default();
        let (tx, mut events) = mpsc::channel(32);
        let (stop, rx) = watch::channel(false);
        let (ready, readiness) = watch::channel(false);
        let job = tokio::spawn(monitor_link(p.clone(), tx, None, None, rx, Some(ready)));
        loop {
            if events.recv().await.unwrap()["event"] == "measurement" {
                break;
            }
        }
        assert!(*readiness.borrow());
        p.0.up.store(false, Ordering::SeqCst);
        tokio::time::advance(Duration::from_secs(3)).await;
        loop {
            if events.recv().await.unwrap()["event"] == "disconnected" {
                break;
            }
        }
        assert!(!*readiness.borrow());
        loop {
            if events.recv().await.unwrap()["event"] == "connected" {
                break;
            }
        }
        assert_eq!(p.0.subscriptions.load(Ordering::SeqCst), 2);
        stop.send(true).unwrap();
        job.await.unwrap().unwrap();
        assert!(!*readiness.borrow());
    }
    #[tokio::test(start_paused = true)]
    async fn hung_connections_timeout_and_exhaust_budget() {
        let p = Mock::default();
        p.0.hang.store(true, Ordering::SeqCst);
        let (tx, _rx) = mpsc::channel(32);
        let (_stop, rx) = watch::channel(false);
        let result = monitor_link(p.clone(), tx, None, None, rx, None).await;
        assert!(result.is_err());
        assert_eq!(p.0.opens.load(Ordering::SeqCst), 6);
        assert_eq!(p.0.closes.load(Ordering::SeqCst), 6);
    }
    #[tokio::test(start_paused = true)]
    async fn quiet_measurements_are_marked_stale() {
        let p = Mock::default();
        let (tx, mut events) = mpsc::channel(32);
        let (stop, rx) = watch::channel(false);
        let job = tokio::spawn(monitor_link(p, tx, None, None, rx, None));
        loop {
            if events.recv().await.unwrap()["event"] == "measurement" {
                break;
            }
        }
        tokio::time::advance(Duration::from_secs(6)).await;
        loop {
            if events.recv().await.unwrap()["event"] == "stale" {
                break;
            }
        }
        stop.send(true).unwrap();
        job.await.unwrap().unwrap();
    }
}
