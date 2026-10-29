//! Owns the Tokio runtime and the riding session; the UI thread never blocks on I/O.
use bike_protocol::Action;
use bike_session::{Actions, Settings, Status, StatusTx};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::{
    runtime::Runtime,
    sync::{broadcast, watch},
    task::JoinHandle,
};

pub struct Backend {
    runtime: Runtime,
    status_tx: StatusTx,
    status: watch::Receiver<Status>,
    actions: Actions,
    session: Option<(watch::Sender<bool>, JoinHandle<()>)>,
}

impl Backend {
    pub fn new(ctx: egui::Context) -> anyhow::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let (status_tx, status) = watch::channel(Status::default());
        let (actions, _) = broadcast::channel(32);
        // Repaint whenever the session publishes something new.
        let mut changes = status.clone();
        runtime.spawn(async move {
            while changes.changed().await.is_ok() {
                ctx.request_repaint();
            }
        });
        Ok(Self {
            runtime,
            status_tx: Arc::new(status_tx),
            status,
            actions,
            session: None,
        })
    }

    /// Sample values for trying the interface without devices; labelled as demo in the UI.
    pub fn show_demo(&self) {
        let gear = bike_gearing::TEMPLATES[0].drivetrain().ok().map(|d| {
            let gears = d.gears();
            bike_session::GearInfo::new(&gears[d.default_gear() - 1], gears.len())
        });
        // Readings stay fresh for an hour of demo.
        let fresh = Some(Instant::now() + Duration::from_secs(3600));
        self.status_tx.send_modify(|s| {
            s.phase = bike_session::Phase::Running;
            s.address = Some(std::net::Ipv4Addr::new(192, 168, 1, 10));
            s.trainer = bike_session::Device {
                name: Some("Demo trainer".into()),
                connected: true,
                battery_percent: None,
            };
            s.controller = bike_session::Device {
                name: Some("Demo controller".into()),
                connected: true,
                battery_percent: Some(93),
            };
            s.heart_rate = bike_session::Device {
                name: Some("Demo HR strap".into()),
                connected: true,
                battery_percent: None,
            };
            s.heart_rate_bpm = Some(142);
            s.heart_rate_at = fresh;
            s.app_trainer_links = 1;
            s.power_w = Some(182);
            s.cadence_rpm = Some(88.0);
            s.speed_kmh = Some(31.4);
            s.telemetry_at = fresh;
            s.gearing_name = Some(bike_gearing::TEMPLATES[0].name.into());
            s.gear = gear;
            s.control = bike_session::Control::Simulation;
            s.grade_percent = Some(2.4);
            s.target_w = Some(185);
        });
    }

    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    pub fn running(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|(_, job)| !job.is_finished())
    }

    pub fn start(&mut self, settings: Settings) {
        if self.running() {
            return;
        }
        // Keep the log across rides; everything else starts fresh.
        self.status_tx.send_modify(|s| {
            let log = std::mem::take(&mut s.log);
            let seq = s.log_seq;
            *s = Status {
                log,
                log_seq: seq,
                ..Status::default()
            };
        });
        let (stop, stopped) = watch::channel(false);
        let job = self.runtime.spawn(run(
            settings,
            self.status_tx.clone(),
            self.actions.clone(),
            stopped,
        ));
        self.session = Some((stop, job));
    }

    pub fn stop(&self) {
        if let Some((stop, _)) = &self.session {
            let _ = stop.send(true);
        }
    }

    /// Run Bluetooth work for the UI (learning a controller) on the backend runtime.
    pub fn spawn(&self, job: impl std::future::Future<Output = ()> + Send + 'static) {
        self.runtime.spawn(job);
    }

    /// On-screen shift buttons and keyboard shortcuts use the same path as the controller.
    pub fn shift(&self, harder: bool) {
        let action = if harder {
            Action::ShiftUp
        } else {
            Action::ShiftDown
        };
        let _ = self.actions.send((Instant::now(), action));
    }

    /// Stop and wait briefly so the trainer is reset before the window disappears.
    pub fn shutdown(&mut self) {
        self.stop();
        if let Some((_, job)) = self.session.take() {
            let _ = self
                .runtime
                .block_on(async { tokio::time::timeout(Duration::from_secs(4), job).await });
        }
    }
}

async fn run(settings: Settings, status: StatusTx, actions: Actions, stop: watch::Receiver<bool>) {
    // Errors are already recorded in the status and log by the session.
    let _ = bike_session::run(settings, status, actions, stop).await;
}
