use anyhow::{Context, Result, bail};
use bike_core::{ControllerMap, PacketMap};
use bike_protocol::Action;
use clap::{Parser, Subcommand};
use std::{
    net::Ipv4Addr,
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::{
    sync::{broadcast, mpsc, watch},
    task::JoinSet,
};
mod gears;
mod start;

/// Scan length before picking a device given by ID or name.
const FIND_SECONDS: u64 = 3;

#[derive(Debug, Parser)]
#[command(
    version,
    about = format!("{}: Bluetooth trainer controls for MyWhoosh. Run without a command to start riding.", bike_core::APP_NAME)
)]
struct Cli {
    #[arg(long, default_value_t = 0, global = true)]
    adapter: usize,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Debug, Subcommand)]
enum Command {
    /// Quick start (the default): find a known controller and connect it to MyWhoosh.
    Start {
        /// LAN IPv4 to serve on; detected automatically when omitted.
        #[arg(long)]
        bind: Option<Ipv4Addr>,
        #[arg(long, default_value_t = 0)]
        seconds: u64,
        /// Test phase: also serve the trainer to MyWhoosh as the virtual trainer "OsmolControl".
        #[arg(long)]
        trainer_bridge: bool,
        /// Virtual shifting with a gear template ID from `bikectl gears` (needs --trainer-bridge).
        #[arg(long, requires = "trainer_bridge")]
        gears: Option<String>,
        /// Rider plus bike mass for the road physics.
        #[arg(long, default_value_t = 84.0, value_parser = mass)]
        mass_kg: f64,
        /// Do not look for a heart rate strap.
        #[arg(long)]
        no_heart_rate: bool,
    },
    /// List Bluetooth adapters.
    Adapters,
    /// Discover devices and advertised services (JSON).
    Scan {
        #[arg(long,default_value_t=10,value_parser=clap::value_parser!(u64).range(1..=120))]
        seconds: u64,
    },
    /// Read GATT services, capabilities and readable parameters; never writes trainer control.
    Inspect { device: String },
    /// Capture an explicitly selected vendor notification UUID without interpreting it.
    Capture {
        device: String,
        characteristic: uuid::Uuid,
        #[arg(long, default_value_t = 60)]
        seconds: u64,
    },
    /// Monitor one or more exact scan IDs/names independently as JSON lines.
    Monitor {
        #[arg(required = true)]
        devices: Vec<String>,
        /// Optional explicit vendor notification mapping. No built-in V100 assumptions.
        #[arg(long)]
        mapping: Option<PathBuf>,
        /// Apply mapping only to this exact device selector; required for multi-device monitor.
        #[arg(long)]
        controller: Option<String>,
        #[arg(long, default_value_t = 0)]
        seconds: u64,
    },
    /// Advertise an OpenBikeControl TCP server for MyWhoosh; optional BLE inputs.
    ConnectMywhoosh {
        /// Concrete local LAN IPv4 address reachable from the MyWhoosh host.
        #[arg(long)]
        bind: Ipv4Addr,
        #[arg(long, default_value_t = bike_session::CONTROLLER_PORT)]
        port: u16,
        #[arg(long, default_value = bike_core::APP_NAME)]
        name: String,
        #[arg(long)]
        no_mdns: bool,
        #[arg(long)]
        trainer: Option<String>,
        #[arg(long)]
        controller: Option<String>,
        #[arg(long, requires = "controller")]
        mapping: Option<PathBuf>,
        /// Read 'up'/'down' lines from stdin for a deliberate manual integration test.
        #[arg(long)]
        stdin_actions: bool,
        #[arg(long, default_value_t = 0)]
        seconds: u64,
    },
    /// Gear templates and calculator. No arguments lists templates; give an ID or --front/--rear.
    Gears {
        /// Template ID from the list.
        template: Option<String>,
        /// Custom chainrings, e.g. 50/34.
        #[arg(long, requires = "rear", conflicts_with = "template")]
        front: Option<String>,
        /// Custom cassette, e.g. 11-12-13-14-15-17-19-21-24-28.
        #[arg(long, requires = "front")]
        rear: Option<String>,
        /// Wheel circumference in mm (default from the template: 2136 road, 2200 gravel).
        #[arg(long)]
        wheel_mm: Option<u16>,
    },
    /// Decode captured standard BLE bytes offline, e.g. decode 2ad2 4400c409b400fa00.
    Decode { characteristic: String, hex: String },
}
fn mass(text: &str) -> Result<f64, String> {
    match text.parse::<f64>() {
        Ok(kg) if (30.0..=200.0).contains(&kg) => Ok(kg),
        _ => Err("mass must be 30-200 kg".into()),
    }
}
fn mapping(path: Option<PathBuf>) -> Result<Option<PacketMap>> {
    path.map(|p| {
        use std::io::Read;
        let file = std::fs::File::open(&p).with_context(|| format!("open {}", p.display()))?;
        let mut bytes = Vec::new();
        file.take(16385)
            .read_to_end(&mut bytes)
            .context("read bounded mapping file")?;
        if bytes.len() > 16384 {
            bail!("mapping exceeds 16 KiB");
        }
        let config: ControllerMap =
            serde_json::from_slice(&bytes).context("parse controller mapping")?;
        let parsed = PacketMap::new(config).map_err(anyhow::Error::msg)?;
        uuid::Uuid::parse_str(&parsed.characteristic)
            .context("mapping characteristic must be a UUID")?;
        Ok(parsed)
    })
    .transpose()
}
fn shutdown(seconds: u64) -> (watch::Sender<bool>, watch::Receiver<bool>) {
    let (tx, rx) = watch::channel(false);
    let sender = tx.clone();
    tokio::spawn(async move {
        if seconds > 0 {
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=tokio::time::sleep(Duration::from_secs(seconds))=>{}}
        } else {
            let _ = tokio::signal::ctrl_c().await;
        }
        let _ = sender.send(true);
    });
    (tx, rx)
}
fn output() -> (mpsc::Sender<serde_json::Value>, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel(128);
    let job = tokio::spawn(async move {
        while let Some(v) = rx.recv().await {
            println!("{v}");
        }
    });
    (tx, job)
}
async fn finish(mut jobs: JoinSet<Result<()>>, stop: watch::Sender<bool>) -> Result<()> {
    let mut errors = vec![];
    while let Some(r) = jobs.join_next().await {
        match r {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::error!(error=%e,"worker failed");
                errors.push(format!("{e:#}"));
            }
            Err(e) => errors.push(e.to_string()),
        }
    }
    let _ = stop.send(true);
    if !errors.is_empty() {
        bail!("{}", errors.join("; "));
    }
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Start {
        bind: None,
        seconds: 0,
        trainer_bridge: false,
        gears: None,
        mass_kg: 84.0,
        no_heart_rate: false,
    });
    // Quick start prints its own friendly status; diagnostics stay available via RUST_LOG.
    let default_filter = if matches!(command, Command::Start { .. }) {
        "warn"
    } else {
        "info"
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default_filter.into()),
        )
        .with_writer(std::io::stderr)
        .init();
    match command {
        Command::Start {
            bind,
            seconds,
            trainer_bridge,
            gears,
            mass_kg,
            no_heart_rate,
        } => {
            start::run(start::Options {
                adapter: cli.adapter,
                bind,
                seconds,
                bridge: trainer_bridge,
                gears,
                mass_kg,
                heart_rate: !no_heart_rate,
            })
            .await?
        }
        Command::Adapters => println!(
            "{}",
            serde_json::to_string_pretty(&bike_ble::adapters().await?)?
        ),
        Command::Scan { seconds } => {
            let a = bike_ble::adapter(cli.adapter).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&bike_ble::scan(&a, seconds).await?)?
            );
        }
        Command::Inspect { device } => {
            let a = bike_ble::adapter(cli.adapter).await?;
            let p = bike_ble::find(&a, &device, FIND_SECONDS).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&bike_ble::inspect(&p).await?)?
            );
        }
        Command::Capture {
            device,
            characteristic,
            seconds,
        } => {
            let a = bike_ble::adapter(cli.adapter).await?;
            let p = bike_ble::find(&a, &device, FIND_SECONDS).await?;
            let (stop, rx) = shutdown(seconds);
            let (tx, printer) = output();
            let result = bike_ble::monitor_with_readiness(
                p,
                tx,
                Some(PacketMap::capture(characteristic.to_string())),
                None,
                rx,
                None,
            )
            .await;
            let _ = stop.send(true);
            printer.await?;
            result?;
        }
        Command::Gears {
            template,
            front,
            rear,
            wheel_mm,
        } => gears::run(
            template.as_deref(),
            front.as_deref(),
            rear.as_deref(),
            wheel_mm,
        )?,
        Command::Decode {
            characteristic,
            hex,
        } => {
            let id = u16::from_str_radix(characteristic.trim_start_matches("0x"), 16)
                .context("characteristic must be four hex digits")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&bike_protocol::characteristic(
                    id,
                    &bike_protocol::unhex(&hex)?
                )?)?
            );
        }
        Command::Monitor {
            devices,
            mapping: map,
            controller,
            seconds,
        } => {
            let map = mapping(map)?;
            if map.is_some() && devices.len() > 1 && controller.is_none() {
                bail!("--controller is required to scope a multi-device mapping");
            }
            if let Some(c) = &controller
                && !devices.contains(c)
            {
                bail!("--controller must match one device selector");
            }
            let a = bike_ble::adapter(cli.adapter).await?;
            let mut selected = vec![];
            for d in &devices {
                selected.push((d.clone(), bike_ble::find(&a, d, FIND_SECONDS).await?));
            }
            let (stop, rx) = shutdown(seconds);
            let (tx, printer) = output();
            let mut jobs = JoinSet::new();
            for (d, p) in selected {
                let m = if controller.as_ref().is_none_or(|c| c == &d) {
                    map.clone()
                } else {
                    None
                };
                jobs.spawn(bike_ble::monitor_with_readiness(
                    p,
                    tx.clone(),
                    m,
                    None,
                    rx.clone(),
                    None,
                ));
            }
            drop(tx);
            let result = finish(jobs, stop).await;
            printer.await?;
            result?;
        }
        Command::ConnectMywhoosh {
            bind,
            port,
            name,
            no_mdns,
            trainer,
            controller,
            mapping: map,
            stdin_actions,
            seconds,
        } => {
            let map = mapping(map)?;
            if controller.is_some() && map.is_none() {
                bail!("controller requires an explicit --mapping derived from its captured events");
            }
            if trainer.is_some() && trainer == controller {
                bail!("trainer and controller selectors must differ");
            }
            let mut selected = vec![];
            if trainer.is_some() || controller.is_some() {
                let a = bike_ble::adapter(cli.adapter).await?;
                if let Some(d) = trainer {
                    selected.push((bike_ble::find(&a, &d, FIND_SECONDS).await?, None));
                }
                if let Some(d) = controller {
                    selected.push((bike_ble::find(&a, &d, FIND_SECONDS).await?, map));
                }
            }
            let (stop, rx) = shutdown(seconds);
            let (tx, printer) = output();
            let (actions, _) = broadcast::channel(32);
            let (ready, readiness) = watch::channel(stdin_actions);
            let mut jobs = JoinSet::new();
            for (p, m) in selected {
                let readiness_sender = if m.is_some() && !stdin_actions {
                    Some(ready.clone())
                } else {
                    None
                };
                jobs.spawn(bike_ble::monitor_with_readiness(
                    p,
                    tx.clone(),
                    m,
                    Some(actions.clone()),
                    rx.clone(),
                    readiness_sender,
                ));
            }
            drop(tx);
            let config = bike_mywhoosh::Config {
                address: bind,
                port,
                name,
                advertise: !no_mdns,
            };
            let s = stop.clone();
            let a = actions.clone();
            let r = rx.clone();
            jobs.spawn(async move {
                let result = bike_mywhoosh::serve_with_readiness(config, a, r, readiness).await;
                let _ = s.send(true);
                result
            });
            // Tokio stdin may block on shutdown, so use a detached standard input thread.
            if stdin_actions {
                let a = actions.clone();
                std::thread::spawn(move || {
                    use std::io::BufRead;
                    for line in std::io::stdin().lock().lines() {
                        let Ok(line) = line else { break };
                        let action = match line.trim() {
                            "up" => Action::ShiftUp,
                            "down" => Action::ShiftDown,
                            _ => continue,
                        };
                        let _ = a.send((Instant::now(), action));
                    }
                });
            }
            let result = finish(jobs, stop).await;
            printer.await?;
            result?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("bikectl").chain(args.iter().copied()))
    }

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn start_is_the_default_and_its_flags_are_checked() {
        assert!(parse(&[]).is_ok_and(|c| c.command.is_none()));
        let Ok(Cli {
            command:
                Some(Command::Start {
                    trainer_bridge,
                    gears,
                    mass_kg,
                    no_heart_rate,
                    ..
                }),
            ..
        }) = parse(&[
            "start",
            "--trainer-bridge",
            "--gears",
            "claris-r2000-50-34-11-32",
            "--mass-kg",
            "77",
            "--no-heart-rate",
        ])
        else {
            panic!("start flags rejected");
        };
        assert!(trainer_bridge && no_heart_rate);
        assert_eq!(gears.as_deref(), Some("claris-r2000-50-34-11-32"));
        assert_eq!(mass_kg, 77.0);
        // Gears need the trainer bridge; implausible masses are refused.
        assert!(parse(&["start", "--gears", "x"]).is_err());
        assert!(parse(&["start", "--mass-kg", "20"]).is_err());
        assert!(parse(&["start", "--mass-kg", "abc"]).is_err());
        assert!(parse(&["start", "--bind", "not-an-ip"]).is_err());
    }

    #[test]
    fn other_commands_parse_with_their_defaults() {
        let Ok(Cli {
            command: Some(Command::ConnectMywhoosh { port, name, .. }),
            adapter,
        }) = parse(&["connect-mywhoosh", "--bind", "192.168.1.10"])
        else {
            panic!("connect-mywhoosh rejected");
        };
        assert_eq!(port, bike_session::CONTROLLER_PORT);
        assert_eq!(name, bike_core::APP_NAME);
        assert_eq!(adapter, 0);
        assert!(parse(&["connect-mywhoosh"]).is_err(), "--bind is required");
        assert!(parse(&["--adapter", "1", "adapters"]).is_ok_and(|c| c.adapter == 1));
        assert!(
            parse(&["gears", "--front", "50/34"]).is_err(),
            "--rear is required"
        );
        assert!(parse(&["gears", "x", "--front", "50", "--rear", "11"]).is_err());
        assert!(parse(&["monitor"]).is_err(), "a device is required");
        assert!(parse(&["decode", "2ad2", "44000000"]).is_ok());
    }
}
