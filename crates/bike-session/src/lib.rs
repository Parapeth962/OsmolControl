//! One riding session shared by the CLI and the GUI: find devices, serve MyWhoosh, and publish a
//! live [`Status`]. No UI code here; front ends only read the status and send actions.
use anyhow::{Context, Result, anyhow, bail};
use bike_core::APP_NAME;
use bike_gearing::{Drivetrain, VirtualShifter};
use bike_protocol::Action;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinSet;

pub mod bridge;

/// DirCon port used by Wahoo trainers; MyWhoosh discovers it over mDNS anyway.
pub const DIRCON_PORT: u16 = 36866;
pub const CONTROLLER_PORT: u16 = 36867;
const LOG_LINES: usize = 200;

/// How OsmolControl works with MyWhoosh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Mode {
    /// OsmolControl is the trainer and does the shifting with the selected gears.
    #[default]
    OsmolGears,
    /// OsmolControl is the trainer; MyWhoosh shifts (controller sent to MyWhoosh).
    VirtualTrainer,
    /// MyWhoosh pairs the trainer itself; OsmolControl only forwards the controller.
    ControllerOnly,
}

impl Mode {
    pub const ALL: [Self; 3] = [Self::OsmolGears, Self::VirtualTrainer, Self::ControllerOnly];

    pub fn label(self) -> &'static str {
        match self {
            Self::OsmolGears => "OsmolControl gears",
            Self::VirtualTrainer => "Virtual trainer, MyWhoosh shifting",
            Self::ControllerOnly => "Controller only",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::OsmolGears => {
                "MyWhoosh pairs OsmolControl as the trainer. Your gear setup decides the resistance. Turn Virtual Shifting off in MyWhoosh."
            }
            Self::VirtualTrainer => {
                "MyWhoosh pairs OsmolControl as the trainer and the controller, and does the shifting itself."
            }
            Self::ControllerOnly => {
                "MyWhoosh pairs the trainer over Bluetooth itself. OsmolControl only connects the controller. Use this where incoming network connections are blocked."
            }
        }
    }

    fn uses_trainer(self) -> bool {
        self != Self::ControllerOnly
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GearSource {
    Template(String),
    Custom(CustomGears),
}

/// A drivetrain entered by the user. Serializes like the former struct variant,
/// so settings saved before `name` existed still load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomGears {
    #[serde(default)]
    pub name: String,
    pub chainrings: Vec<u16>,
    pub cassette: Vec<u16>,
    pub wheel_mm: u16,
}

impl CustomGears {
    pub fn display_name(&self) -> &str {
        if self.name.trim().is_empty() {
            "Custom"
        } else {
            self.name.trim()
        }
    }

    pub fn drivetrain(&self) -> Result<Drivetrain, bike_gearing::GearError> {
        Drivetrain::new(&self.chainrings, &self.cassette, self.wheel_mm)
    }
}

/// Share of the road grade MyWhoosh sends to the trainer (its trainer difficulty): the grade
/// it writes over FTMS is half of the one on its screen.
pub const MYWHOOSH_GRADE_SHARE: f64 = 0.5;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub mode: Mode,
    pub gears: GearSource,
    /// Rider plus bike.
    pub mass_kg: f64,
    /// Climb difficulty for virtual shifting, in percent of the real road grade (100 = as on
    /// the map; MyWhoosh's own trainer difficulty is neutralized).
    pub difficulty_percent: f64,
    pub adapter: usize,
    /// LAN address to serve on; detected when `None`.
    pub bind: Option<Ipv4Addr>,
    /// Named custom drivetrains kept for later rides.
    pub my_setups: Vec<CustomGears>,
    /// Template ids starred by the user.
    pub favorites: Vec<String>,
    /// Label of the one controller profile to look for; any known controller when `None`.
    pub controller: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::default(),
            gears: GearSource::Template(bike_gearing::TEMPLATES[0].id.into()),
            mass_kg: 84.0,
            difficulty_percent: 100.0,
            adapter: 0,
            bind: None,
            my_setups: Vec::new(),
            favorites: Vec::new(),
            controller: None,
        }
    }
}

impl Settings {
    /// The selected drivetrain and a display name.
    pub fn drivetrain(&self) -> Result<(String, Drivetrain)> {
        match &self.gears {
            GearSource::Template(id) => {
                let t = bike_gearing::template(id)
                    .ok_or_else(|| anyhow!("unknown gear template {id:?}"))?;
                Ok((t.name.into(), t.drivetrain()?))
            }
            GearSource::Custom(c) => Ok((c.display_name().into(), c.drivetrain()?)),
        }
    }

    pub fn is_favorite(&self, template_id: &str) -> bool {
        self.favorites.iter().any(|f| f == template_id)
    }

    /// Star or unstar a template.
    pub fn toggle_favorite(&mut self, template_id: &str) {
        if self.is_favorite(template_id) {
            self.favorites.retain(|f| f != template_id);
        } else {
            self.favorites.push(template_id.into());
        }
    }

    /// Store `setup` under its name, replacing a saved setup with the same name.
    pub fn save_setup(&mut self, setup: CustomGears) {
        match self
            .my_setups
            .iter_mut()
            .find(|s| s.display_name() == setup.display_name())
        {
            Some(existing) => *existing = setup,
            None => self.my_setups.push(setup),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Idle,
    SearchingTrainer,
    SearchingController,
    Running,
    Stopped,
}

/// What the trainer is currently asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Control {
    #[default]
    None,
    /// Road simulation; with virtual shifting OsmolControl turns it into power targets.
    Simulation,
    /// The app set power (ERG) or resistance itself; passed through, gears ignored.
    Target,
}

#[derive(Debug, Clone, Default)]
pub struct Device {
    pub name: Option<String>,
    pub connected: bool,
    pub battery_percent: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GearInfo {
    pub number: usize,
    pub of: usize,
    pub chainring: u16,
    pub cog: u16,
    pub ratio: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Status {
    pub phase: Phase,
    pub error: Option<String>,
    pub mode: Mode,
    pub address: Option<Ipv4Addr>,
    pub trainer: Device,
    pub controller: Device,
    /// Heart rate strap; its readings go to MyWhoosh through the virtual trainer.
    pub heart_rate: Device,
    /// MyWhoosh connections to the virtual trainer and to the controller service.
    pub app_trainer_links: usize,
    pub app_controller_links: usize,
    pub power_w: Option<i16>,
    pub cadence_rpm: Option<f64>,
    pub speed_kmh: Option<f64>,
    pub telemetry_at: Option<Instant>,
    pub heart_rate_bpm: Option<u16>,
    pub heart_rate_at: Option<Instant>,
    pub gearing_name: Option<String>,
    pub gear: Option<GearInfo>,
    pub control: Control,
    pub grade_percent: Option<f64>,
    pub target_w: Option<i16>,
    pub last_action: Option<(Action, Instant)>,
    pub log: VecDeque<String>,
    /// Total log lines ever written, so readers can print only new ones.
    pub log_seq: u64,
}

impl Status {
    /// Telemetry is shown only while fresh.
    pub fn telemetry_fresh(&self) -> bool {
        self.telemetry_at
            .is_some_and(|t| t.elapsed() < Duration::from_secs(3))
    }

    /// Heart rate is shown only while fresh.
    pub fn heart_rate_fresh(&self) -> bool {
        self.heart_rate_at
            .is_some_and(|t| t.elapsed() < Duration::from_secs(5))
    }
}

pub type StatusTx = Arc<watch::Sender<Status>>;
pub type Actions = broadcast::Sender<(Instant, Action)>;

pub fn log(status: &StatusTx, line: impl Into<String>) {
    let line = line.into();
    tracing::info!("{line}");
    status.send_modify(|s| {
        if s.log.len() >= LOG_LINES {
            s.log.pop_front();
        }
        s.log.push_back(line);
        s.log_seq += 1;
    });
}

/// The local address of the default route. UDP `connect` sends no packet.
pub fn lan_address() -> Result<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).context("open probe socket")?;
    socket
        .connect((Ipv4Addr::new(192, 0, 2, 1), 9))
        .context("no network route; connect to your LAN or set the address in Settings")?;
    match socket.local_addr()?.ip() {
        IpAddr::V4(ip) if ip.is_private() => Ok(ip),
        ip => {
            bail!("default route uses {ip}, not a private LAN address; set the address in Settings")
        }
    }
}

#[cfg(target_os = "linux")]
fn firewall_hint(status: &StatusTx, address: Ipv4Addr, mode: Mode) {
    let enabled = std::fs::read_to_string("/etc/ufw/ufw.conf")
        .is_ok_and(|c| c.lines().any(|l| l.trim() == "ENABLED=yes"));
    if enabled {
        let ports = match mode {
            Mode::OsmolGears => DIRCON_PORT.to_string(),
            Mode::VirtualTrainer => format!("{DIRCON_PORT},{CONTROLLER_PORT}"),
            Mode::ControllerOnly => CONTROLLER_PORT.to_string(),
        };
        log(
            status,
            format!(
                "ufw is enabled. If MyWhoosh cannot connect, allow it once: sudo ufw allow proto tcp to {address} port {ports} comment '{APP_NAME}'"
            ),
        );
    }
}
#[cfg(windows)]
fn firewall_hint(status: &StatusTx, _: Ipv4Addr, _: Mode) {
    log(
        status,
        format!("If Windows Firewall asks, allow {APP_NAME} on Private networks."),
    );
}
#[cfg(not(any(target_os = "linux", windows)))]
fn firewall_hint(_: &StatusTx, _: Ipv4Addr, _: Mode) {}

/// Repeat bounded scans until something is found or the session is stopped.
async fn search<T, F, Fut>(
    stop: &mut watch::Receiver<bool>,
    status: &StatusTx,
    waiting: &str,
    mut scan: F,
) -> Result<Option<T>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Option<T>>>,
{
    loop {
        if *stop.borrow() {
            return Ok(None);
        }
        tokio::select! {
            found = scan() => if let Some(found) = found? { return Ok(Some(found)) },
            _ = stop.changed() => return Ok(None),
        }
        log(status, waiting);
    }
}

/// Mirror a client counter into the status.
fn watch_links(
    jobs: &mut JoinSet<Result<()>>,
    status: &StatusTx,
    mut rx: watch::Receiver<usize>,
    apply: fn(&mut Status, usize),
) {
    let status = status.clone();
    jobs.spawn(async move {
        while rx.changed().await.is_ok() {
            let n = *rx.borrow();
            status.send_modify(|s| apply(s, n));
        }
        Ok(())
    });
}

/// Turn controller monitor events into status and log lines.
fn controller_events(
    jobs: &mut JoinSet<Result<()>>,
    status: &StatusTx,
    mut events: mpsc::Receiver<serde_json::Value>,
    shifting: bool,
) {
    let status = status.clone();
    jobs.spawn(async move {
        while let Some(e) = events.recv().await {
            match e["event"].as_str() {
                Some("connected") => {
                    status.send_modify(|s| s.controller.connected = true);
                    log(&status, "Controller connected.");
                }
                Some("disconnected") => {
                    status.send_modify(|s| s.controller.connected = false);
                    log(&status, "Controller disconnected; reconnecting...");
                }
                Some("controller_action") => {
                    let action = match e["action"].as_str() {
                        Some("shift_up") => Action::ShiftUp,
                        Some("shift_down") => Action::ShiftDown,
                        _ => continue,
                    };
                    status.send_modify(|s| s.last_action = Some((action, Instant::now())));
                    if !shifting {
                        let word = if action == Action::ShiftUp {
                            "harder"
                        } else {
                            "easier"
                        };
                        log(&status, format!("Shift: {word}"));
                    }
                }
                Some("measurement") => {
                    if let Some(b) = e["value"]["battery_percent"].as_u64() {
                        status.send_modify(|s| s.controller.battery_percent = u8::try_from(b).ok());
                    }
                }
                _ => {}
            }
        }
        Ok(())
    });
}

/// Where users add controller profiles: `controllers.json` in the app's config folder
/// (`%APPDATA%\OsmolControl` on Windows, `$XDG_CONFIG_HOME/osmolcontrol` or
/// `~/.config/osmolcontrol` elsewhere).
pub fn controllers_file() -> Option<std::path::PathBuf> {
    let dir = if cfg!(windows) {
        std::path::PathBuf::from(std::env::var_os("APPDATA")?).join(APP_NAME)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
            })?
            .join("osmolcontrol")
    };
    Some(dir.join("controllers.json"))
}

/// The user's profiles from [`controllers_file`]; none when the file does not exist yet.
pub fn user_controllers() -> Result<Vec<bike_core::ControllerProfile>> {
    read_controllers(&user_file()?)
}

/// Add `profile` to the user's file, replacing one with the same label. A broken file is an
/// error rather than being overwritten.
pub fn save_controller(profile: bike_core::ControllerProfile) -> Result<std::path::PathBuf> {
    let path = user_file()?;
    save_controller_in(&path, profile)?;
    Ok(path)
}

/// Remove the user's profile called `label`.
pub fn remove_controller(label: &str) -> Result<std::path::PathBuf> {
    let path = user_file()?;
    let mut profiles = read_controllers(&path)?;
    profiles.retain(|p| p.label != label);
    write_controllers(&path, &profiles)?;
    Ok(path)
}

fn user_file() -> Result<std::path::PathBuf> {
    controllers_file().ok_or_else(|| anyhow!("no config folder for controllers.json"))
}

fn read_controllers(path: &std::path::Path) -> Result<Vec<bike_core::ControllerProfile>> {
    match std::fs::read_to_string(path) {
        Ok(json) => {
            bike_core::parse_controllers(&json).map_err(|e| anyhow!("{}: {e}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(anyhow!("cannot read {}: {e}", path.display())),
    }
}

fn save_controller_in(path: &std::path::Path, profile: bike_core::ControllerProfile) -> Result<()> {
    profile.check().map_err(anyhow::Error::msg)?;
    let mut profiles = read_controllers(path)?;
    match profiles.iter_mut().find(|p| p.label == profile.label) {
        Some(existing) => *existing = profile,
        None => profiles.push(profile),
    }
    write_controllers(path, &profiles)
}

fn write_controllers(
    path: &std::path::Path,
    profiles: &[bike_core::ControllerProfile],
) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(profiles)?;
    std::fs::write(path, json + "\n").with_context(|| format!("write {}", path.display()))
}

/// User profiles first, so they can override a built-in one; a broken user file is reported
/// and skipped. With `only`, just that profile (all of them if it no longer exists).
fn controller_profiles(status: &StatusTx, only: Option<&str>) -> Vec<bike_core::ControllerProfile> {
    let mut profiles = bike_core::builtin_controllers();
    match user_controllers() {
        Ok(user) if user.is_empty() => {}
        Ok(user) => {
            log(
                status,
                format!("Loaded {} controller profile(s) of your own.", user.len()),
            );
            profiles.splice(0..0, user);
        }
        Err(e) => log(status, format!("Ignoring your controller profiles: {e:#}")),
    }
    if let Some(label) = only {
        if profiles.iter().any(|p| p.label == label) {
            profiles.retain(|p| p.label == label);
        } else {
            log(
                status,
                format!("Controller {label:?} is no longer defined; looking for any known one."),
            );
        }
    }
    profiles
}

/// Run until `stop` fires or a server fails. The status is updated throughout; `actions`
/// carries controller presses and may also be fed by the UI (on-screen shift buttons).
pub async fn run(
    settings: Settings,
    status: StatusTx,
    actions: Actions,
    stop: watch::Receiver<bool>,
) -> Result<()> {
    let result = session(&settings, &status, actions, stop).await;
    status.send_modify(|s| {
        s.phase = Phase::Stopped;
        s.trainer.connected = false;
        s.controller.connected = false;
        s.heart_rate.connected = false;
        s.app_trainer_links = 0;
        s.app_controller_links = 0;
        s.error = result.as_ref().err().map(|e| format!("{e:#}"));
    });
    match &result {
        Ok(()) => log(&status, format!("{APP_NAME} stopped.")),
        Err(e) => log(&status, format!("Stopped with an error: {e:#}")),
    }
    result
}

async fn session(
    settings: &Settings,
    status: &StatusTx,
    actions: Actions,
    mut external_stop: watch::Receiver<bool>,
) -> Result<()> {
    let mode = settings.mode;
    let shifter = if mode == Mode::OsmolGears {
        let (name, drivetrain) = settings.drivetrain()?;
        let count = drivetrain.chainrings.len() * drivetrain.cassette.len();
        log(status, format!("Virtual shifting: {name} ({count} gears)."));
        status.send_modify(|s| s.gearing_name = Some(name));
        Some(
            VirtualShifter::new(drivetrain, settings.mass_kg)
                .with_grade_scale(settings.difficulty_percent / 100.0 / MYWHOOSH_GRADE_SHARE),
        )
    } else {
        None
    };
    let address = match settings.bind {
        Some(a) => a,
        None => lan_address()?,
    };
    status.send_modify(|s| {
        s.mode = mode;
        s.address = Some(address);
        s.error = None;
    });
    log(status, format!("{APP_NAME} is starting on {address}."));
    let adapter = bike_ble::adapter(settings.adapter).await?;

    // Internal stop: the user's stop or a failing server ends everything.
    let (stop, stopped) = watch::channel(false);
    {
        let stop = stop.clone();
        tokio::spawn(async move {
            while !*external_stop.borrow() {
                if external_stop.changed().await.is_err() {
                    break;
                }
            }
            let _ = stop.send(true);
        });
    }
    let mut waiting = stopped.clone();

    // Scans run one after another: concurrent discovery on one adapter fails.
    let trainer = if mode.uses_trainer() {
        status.send_modify(|s| s.phase = Phase::SearchingTrainer);
        log(
            status,
            "Looking for your trainer... pedal a few strokes to wake it up.",
        );
        let found = search(
            &mut waiting,
            status,
            "Still looking for a trainer (any FTMS smart trainer).",
            || bike_ble::trainer::find_trainer(&adapter, 5),
        )
        .await?;
        let Some((p, name)) = found else {
            return Ok(());
        };
        let snapshot = bike_ble::trainer::snapshot(&p, name).await?;
        log(
            status,
            format!(
                "Found trainer {} (power {}-{} W).",
                snapshot.name, snapshot.limits.power_w.0, snapshot.limits.power_w.1
            ),
        );
        status.send_modify(|s| s.trainer.name = Some(snapshot.name.clone()));
        Some((p, snapshot))
    } else {
        None
    };
    status.send_modify(|s| s.phase = Phase::SearchingController);
    log(
        status,
        "Looking for your controller... press any button on it to wake it up.",
    );
    let profiles = controller_profiles(status, settings.controller.as_deref());
    let controller = loop {
        if *waiting.borrow() {
            return Ok(());
        }
        if let Some(found) = bike_ble::find_known_controller(&adapter, 5, &profiles).await? {
            break Some(found);
        }
        if trainer.is_some() {
            log(
                status,
                "No controller found; continuing without one (use the on-screen buttons).",
            );
            break None;
        }
        log(status, "Still looking for a controller.");
    };

    // Optional: one scan, and the ride goes on without a strap.
    let strap = if trainer.is_some() {
        match bike_ble::find_heart_rate(&adapter, 5).await {
            Ok(Some((p, name))) => {
                log(status, format!("Found heart rate sensor {name}."));
                status.send_modify(|s| s.heart_rate.name = Some(name));
                Some(p)
            }
            Ok(None) => {
                log(
                    status,
                    "No heart rate sensor found; continuing without one.",
                );
                None
            }
            Err(e) => {
                log(status, format!("Heart rate sensor search failed: {e:#}"));
                None
            }
        }
    } else {
        None
    };

    let mut jobs = JoinSet::new();
    let shifting = shifter.is_some();
    if let Some((peripheral, profile, name)) = controller {
        log(status, format!("Found {} ({name}).", profile.label));
        status.send_modify(|s| s.controller.name = Some(profile.label.clone()));
        let map = profile.packet_map().map_err(anyhow::Error::msg)?;
        let (events, event_rx) = mpsc::channel(128);
        controller_events(&mut jobs, status, event_rx, shifting);
        jobs.spawn(bike_ble::monitor_with_readiness(
            peripheral,
            events,
            Some(map),
            Some(actions.clone()),
            stopped.clone(),
            None,
        ));
    }
    if mode != Mode::OsmolGears {
        // MyWhoosh shifts: serve the controller over OpenBikeControl, ready once it is connected.
        let config = bike_mywhoosh::Config {
            address,
            port: CONTROLLER_PORT,
            name: APP_NAME.into(),
            advertise: true,
        };
        let (ready_tx, readiness) = watch::channel(false);
        let mut controller_state = status.subscribe();
        jobs.spawn(async move {
            loop {
                let connected = controller_state.borrow_and_update().controller.connected;
                let _ = ready_tx.send(connected);
                if controller_state.changed().await.is_err() {
                    return Ok(());
                }
            }
        });
        let (active, links) = watch::channel(0);
        watch_links(&mut jobs, status, links, |s, n| s.app_controller_links = n);
        let (s, r, a) = (stop.clone(), stopped.clone(), actions.clone());
        jobs.spawn(async move {
            let result = bike_mywhoosh::serve_with_clients(config, a, r, readiness, active).await;
            let _ = s.send(true);
            result
        });
    }
    if let Some((peripheral, snapshot)) = trainer {
        let (from_trainer, from_trainer_rx) = broadcast::channel(64);
        let (to_app, _) = broadcast::channel(64);
        let (write_tx, app_writes) = mpsc::channel(16);
        let (to_trainer, commands) = mpsc::channel(16);
        let (connected, mut trainer_state) = watch::channel(false);
        {
            let status = status.clone();
            jobs.spawn(async move {
                while trainer_state.changed().await.is_ok() {
                    let up = *trainer_state.borrow();
                    status.send_modify(|s| s.trainer.connected = up);
                    log(
                        &status,
                        if up {
                            "Trainer connected."
                        } else {
                            "Trainer disconnected; reconnecting..."
                        },
                    );
                }
                Ok(())
            });
        }
        jobs.spawn(bike_ble::trainer::run(
            peripheral,
            from_trainer,
            commands,
            stopped.clone(),
            connected,
        ));
        let router = bridge::Router::new(snapshot.limits, shifter);
        let channels = bridge::Channels {
            app_writes,
            from_trainer: from_trainer_rx,
            to_app: to_app.clone(),
            to_trainer,
            actions: shifting.then(|| actions.subscribe()),
            status: status.clone(),
        };
        jobs.spawn(bridge::run(router, channels, stopped.clone()));
        let config = bike_dircon::Config {
            address,
            port: DIRCON_PORT,
            name: APP_NAME.into(),
            serial: "1".into(),
            advertise: true,
        };
        let mut gatt = bridge::mirror(&snapshot);
        if let Some(p) = strap {
            gatt.services.push(bridge::heart_rate_service());
            heart_rate(&mut jobs, status, p, to_app.clone(), stopped.clone());
        }
        let (active, links) = watch::channel(0);
        watch_links(&mut jobs, status, links, |s, n| s.app_trainer_links = n);
        let (s, r) = (stop.clone(), stopped.clone());
        jobs.spawn(async move {
            let result = bike_dircon::serve(config, gatt, to_app, write_tx, r, active).await;
            let _ = s.send(true);
            result
        });
    }
    status.send_modify(|s| s.phase = Phase::Running);
    log(status, "Ready. In MyWhoosh open Device Connection:");
    match mode {
        Mode::OsmolGears => {
            log(
                status,
                format!(
                    "  Trainer: tap LAN and pair \"{APP_NAME}\". Turn OFF Virtual Shifting in MyWhoosh's settings."
                ),
            );
        }
        Mode::VirtualTrainer => {
            log(
                status,
                format!("  Trainer: tap LAN and pair \"{APP_NAME}\"."),
            );
            log(
                status,
                format!("  Controller: tap the controller icon (top right) -> {APP_NAME}."),
            );
        }
        Mode::ControllerOnly => {
            log(
                status,
                "  Pair your trainer as usual and keep MyWhoosh's Virtual Shifting on.",
            );
            log(
                status,
                format!("  Controller: tap the controller icon (top right) -> {APP_NAME}."),
            );
        }
    }
    firewall_hint(status, address, mode);
    finish(jobs, stop).await
}

/// Relay the strap's measurements to the app. Losing the strap is logged and never ends the ride.
fn heart_rate(
    jobs: &mut JoinSet<Result<()>>,
    status: &StatusTx,
    strap: bike_ble::Peripheral,
    to_app: broadcast::Sender<(uuid::Uuid, Vec<u8>)>,
    stop: watch::Receiver<bool>,
) {
    let (events, mut event_rx) = mpsc::channel(64);
    let monitor_status = status.clone();
    jobs.spawn(async move {
        if let Err(e) = bike_ble::monitor(strap, events, None, None, stop).await {
            log(&monitor_status, format!("Heart rate sensor lost: {e:#}"));
        }
        Ok(())
    });
    let status = status.clone();
    jobs.spawn(async move {
        while let Some(e) = event_rx.recv().await {
            match e["event"].as_str() {
                Some("connected") => {
                    status.send_modify(|s| s.heart_rate.connected = true);
                    log(&status, "Heart rate sensor connected.");
                }
                Some("disconnected") => {
                    status.send_modify(|s| s.heart_rate.connected = false);
                    log(&status, "Heart rate sensor disconnected; reconnecting...");
                }
                Some("measurement") => {
                    if let Some(bpm) = e["value"]["heart_rate_bpm"].as_u64() {
                        status.send_modify(|s| {
                            s.heart_rate_bpm = u16::try_from(bpm).ok();
                            s.heart_rate_at = Some(Instant::now());
                        });
                    }
                    if e["characteristic"].as_str()
                        == Some(&bridge::HEART_RATE_MEASUREMENT.to_string())
                        && let Some(raw) = e["raw_hex"].as_str()
                        && let Ok(bytes) = bike_protocol::unhex(raw)
                    {
                        let _ = to_app.send((bridge::HEART_RATE_MEASUREMENT, bytes));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    });
}

async fn finish(mut jobs: JoinSet<Result<()>>, stop: watch::Sender<bool>) -> Result<()> {
    let mut errors = vec![];
    while let Some(r) = jobs.join_next().await {
        match r {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::error!(error = %e, "worker failed");
                errors.push(format!("{e:#}"));
                let _ = stop.send(true);
            }
            Err(e) => errors.push(e.to_string()),
        }
    }
    if !errors.is_empty() {
        bail!("{}", errors.join("; "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learned_controllers_are_saved_replaced_and_kept_safe() {
        let dir = std::env::temp_dir().join(format!("osmol-controllers-{}", std::process::id()));
        let path = dir.join("nested").join("controllers.json");
        let profile = |label: &str, up: &str| bike_core::ControllerProfile {
            label: label.into(),
            name_prefixes: vec!["ACME".into()],
            services: Vec::new(),
            characteristic: "0000aaaa-0000-1000-8000-00805f9b34fb".into(),
            shift_up_hex: up.into(),
            shift_down_hex: "02".into(),
            buttons: None,
            evidence: String::new(),
        };
        assert!(read_controllers(&path).is_ok_and(|p| p.is_empty()));
        assert!(save_controller_in(&path, profile("A", "01")).is_ok());
        assert!(save_controller_in(&path, profile("B", "01")).is_ok());
        assert!(save_controller_in(&path, profile("A", "03")).is_ok());
        let saved = read_controllers(&path).unwrap_or_default();
        assert_eq!(saved, vec![profile("A", "03"), profile("B", "01")]);
        // Invalid profiles are refused, and a hand-edited broken file is never overwritten.
        assert!(save_controller_in(&path, profile("C", "02")).is_err());
        assert!(std::fs::write(&path, "[{").is_ok());
        assert!(save_controller_in(&path, profile("D", "01")).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap_or_default(), "[{");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_settings_round_trip_and_build_gears() {
        let s = Settings::default();
        let json = serde_json::to_string(&s).unwrap_or_default();
        let back: Settings = serde_json::from_str(&json).unwrap_or_default();
        assert_eq!(back, s);
        let partial: Settings = serde_json::from_str(r#"{"mass_kg": 70.0}"#).unwrap_or_default();
        assert_eq!((partial.mass_kg, partial.mode), (70.0, Mode::OsmolGears));
        assert_eq!(partial.controller, None);
        assert!(s.drivetrain().is_ok());
        let custom = Settings {
            gears: GearSource::Custom(CustomGears {
                name: String::new(),
                chainrings: vec![50, 34],
                cassette: vec![11, 13, 15, 18, 21, 24, 28, 32],
                wheel_mm: 2136,
            }),
            ..Settings::default()
        };
        assert_eq!(
            custom.drivetrain().map(|(n, d)| (n, d.gears().len())).ok(),
            Some(("Custom".into(), 16))
        );
    }

    #[test]
    fn custom_gears_saved_before_names_still_load() {
        let old = r#"{"gears":{"Custom":{"chainrings":[50,34],"cassette":[11,14,17,21,28],"wheel_mm":2136}}}"#;
        let s: Settings = serde_json::from_str(old).unwrap_or_default();
        assert_eq!(s.drivetrain().map(|(n, _)| n).ok(), Some("Custom".into()));
        assert!(s.my_setups.is_empty());
    }

    #[test]
    fn favorites_toggle_and_persist() {
        let mut s = Settings::default();
        s.toggle_favorite("wide-40");
        s.toggle_favorite("campa-ekar-40-9-36");
        s.toggle_favorite("wide-40");
        assert_eq!(s.favorites, ["campa-ekar-40-9-36"]);
        assert!(s.is_favorite("campa-ekar-40-9-36") && !s.is_favorite("wide-40"));
        let json = serde_json::to_string(&s).unwrap_or_default();
        assert_eq!(serde_json::from_str::<Settings>(&json).ok(), Some(s));
    }

    #[test]
    fn saving_a_setup_replaces_one_with_the_same_name() {
        let setup = |name: &str, wheel_mm| CustomGears {
            name: name.into(),
            chainrings: vec![40],
            cassette: vec![11, 14, 18, 24, 32, 42],
            wheel_mm,
        };
        let mut s = Settings::default();
        s.save_setup(setup("Gravel", 2200));
        s.save_setup(setup("Road", 2105));
        s.save_setup(setup(" Gravel ", 2250));
        let saved: Vec<_> = s
            .my_setups
            .iter()
            .map(|c| (c.display_name(), c.wheel_mm))
            .collect();
        assert_eq!(saved, [("Gravel", 2250), ("Road", 2105)]);
        let json = serde_json::to_string(&s).unwrap_or_default();
        assert_eq!(serde_json::from_str::<Settings>(&json).ok(), Some(s));
    }

    #[test]
    fn log_is_bounded_and_counted() {
        let (tx, rx) = watch::channel(Status::default());
        let tx = Arc::new(tx);
        for i in 0..(LOG_LINES + 5) {
            log(&tx, format!("line {i}"));
        }
        let s = rx.borrow();
        assert_eq!(s.log.len(), LOG_LINES);
        assert_eq!(s.log_seq, (LOG_LINES + 5) as u64);
        assert_eq!(s.log.back().map(String::as_str), Some("line 204"));
    }
}
