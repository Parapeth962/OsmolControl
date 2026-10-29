//! Terminal front end for a riding session: print the session log until Ctrl+C.
use anyhow::Result;
use bike_session::{GearSource, Mode, Settings, Status};
use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::sync::{broadcast, watch};

pub struct Options {
    pub adapter: usize,
    pub bind: Option<Ipv4Addr>,
    pub seconds: u64,
    /// Serve the real trainer to MyWhoosh as a virtual DirCon trainer.
    pub bridge: bool,
    /// Virtual shifting with this gear template (requires `bridge`).
    pub gears: Option<String>,
    pub mass_kg: f64,
    /// Look for a heart rate strap.
    pub heart_rate: bool,
}

fn settings(options: &Options) -> Settings {
    let mode = match (options.bridge, &options.gears) {
        (true, Some(_)) => Mode::OsmolGears,
        (true, None) => Mode::VirtualTrainer,
        (false, _) => Mode::ControllerOnly,
    };
    let defaults = Settings::default();
    Settings {
        mode,
        gears: options
            .gears
            .clone()
            .map_or(defaults.gears, GearSource::Template),
        mass_kg: options.mass_kg,
        adapter: options.adapter,
        bind: options.bind,
        heart_rate: options.heart_rate,
        ..defaults
    }
}

pub async fn run(options: Options) -> Result<()> {
    let (status_tx, mut status) = watch::channel(Status::default());
    let (actions, _) = broadcast::channel(32);
    let (stop, stopped) = crate::shutdown(options.seconds);
    let printer = tokio::spawn(async move {
        let mut printed = 0;
        loop {
            {
                let s = status.borrow_and_update();
                let new = usize::try_from(s.log_seq.saturating_sub(printed)).unwrap_or(usize::MAX);
                for line in s.log.iter().skip(s.log.len().saturating_sub(new)) {
                    println!("{line}");
                }
                printed = s.log_seq;
            }
            if status.changed().await.is_err() {
                break;
            }
        }
    });
    let result = bike_session::run(settings(&options), Arc::new(status_tx), actions, stopped).await;
    let _ = stop.send(true);
    printer.await?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flags_select_mode() {
        let o = |bridge, gears: Option<&str>| Options {
            adapter: 0,
            bind: None,
            seconds: 0,
            bridge,
            gears: gears.map(Into::into),
            mass_kg: 80.0,
            heart_rate: true,
        };
        assert_eq!(settings(&o(false, None)).mode, Mode::ControllerOnly);
        assert_eq!(settings(&o(true, None)).mode, Mode::VirtualTrainer);
        let s = settings(&o(true, Some("grx-rx400-46-30-11-36")));
        assert_eq!(s.mode, Mode::OsmolGears);
        assert_eq!(
            s.gears,
            GearSource::Template("grx-rx400-46-30-11-36".into())
        );
        assert_eq!(s.mass_kg, 80.0);
        assert!(s.heart_rate);
        let off = Options {
            heart_rate: false,
            ..o(true, None)
        };
        assert!(!settings(&off).heart_rate);
    }
}
