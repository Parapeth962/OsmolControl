//! `bikectl gears`: template list and gear calculator table.
use anyhow::{Context, Result, anyhow};
use bike_gearing::{Category, Drivetrain, TEMPLATES, parse_teeth, step_percent, template};

pub fn run(
    id: Option<&str>,
    front: Option<&str>,
    rear: Option<&str>,
    wheel: Option<u16>,
) -> Result<()> {
    let (title, drivetrain) = match (id, front, rear) {
        (Some(id), _, _) => {
            let t = template(id).ok_or_else(|| {
                anyhow!("unknown template {id:?}; run `bikectl gears` for the list")
            })?;
            (
                t.name.to_owned(),
                Drivetrain::new(
                    t.chainrings,
                    t.cassette,
                    wheel.unwrap_or(t.wheel_circumference_mm),
                )?,
            )
        }
        (None, Some(f), Some(r)) => {
            let f = parse_teeth(f).context("--front: use e.g. 50/34")?;
            let r = parse_teeth(r).context("--rear: use e.g. 11-12-13-14-15-17-19-21-24-28")?;
            (
                "Custom".to_owned(),
                Drivetrain::new(&f, &r, wheel.unwrap_or(2136))?,
            )
        }
        _ => {
            list();
            return Ok(());
        }
    };
    table(&title, &drivetrain);
    Ok(())
}

fn list() {
    for category in Category::ALL {
        out!("{}:", category.label());
        for t in TEMPLATES.iter().filter(|t| t.category == category) {
            let default = if std::ptr::eq(t, &TEMPLATES[0]) {
                "  (default)"
            } else {
                ""
            };
            out!(
                "  {:<28} {:<9} {}{default}",
                t.id,
                bike_gearing::gear_count(t.chainrings.len() * t.cassette.len()),
                t.name
            );
        }
    }
    out!(
        "\nShow one: bikectl gears <ID>    Custom: bikectl gears --front 50/34 --rear 11-12-13-14-15-17-19-21-24-28"
    );
}

fn table(title: &str, d: &Drivetrain) {
    let gears = d.gears();
    out!(
        "{title}: {} gears, wheel {} mm, start in gear {}",
        gears.len(),
        d.wheel_circumference_mm,
        d.default_gear()
    );
    out!(
        "{:>4}  {:>7}  {:>5}  {:>7}  {:>9}  {:>9}  {:>6}",
        "gear",
        "teeth",
        "ratio",
        "m/rev",
        "km/h @60",
        "km/h @90",
        "step"
    );
    for (i, g) in gears.iter().enumerate() {
        let step = i.checked_sub(1).map_or(String::new(), |p| {
            format!("{:+.1}%", step_percent(&gears[p], g))
        });
        out!(
            "{:>4}  {:>7}  {:>5.2}  {:>7.2}  {:>9.1}  {:>9.1}  {:>6}",
            g.number,
            format!("{}x{}", g.chainring, g.cog),
            g.ratio,
            d.development_m(g),
            d.speed_kmh(g, 60.0),
            d.speed_kmh(g, 90.0),
            step
        );
    }
}
