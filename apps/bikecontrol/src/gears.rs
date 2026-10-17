//! Gears tab: template library, custom setup and the calculator table.
use crate::{ACCENT, MUTED, WARN};
use bike_gearing::{
    Brand, Category, Drivetrain, Filter, TEMPLATES, Template, parse_teeth, step_percent,
};
use bike_session::{CustomGears, GearSource, Settings};
use eframe::egui::{self, RichText};

/// Which template list is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Favorites,
    Category(Category),
    All,
}

pub struct GearsTab {
    view: View,
    filter: Filter,
    /// Whether the fields were filled from the saved selection yet.
    loaded: bool,
    /// Whether the first view was chosen (favorites when there are some).
    loaded_view: bool,
    name: String,
    front: String,
    rear: String,
    wheel_mm: u16,
    error: Option<String>,
}

impl Default for GearsTab {
    fn default() -> Self {
        Self {
            view: View::Category(Category::Road),
            filter: Filter::default(),
            loaded: false,
            loaded_view: false,
            name: String::new(),
            front: "50/34".into(),
            rear: "11-12-13-14-15-17-19-21-24-28".into(),
            wheel_mm: 2136,
            error: None,
        }
    }
}

impl GearsTab {
    pub fn show(&mut self, ui: &mut egui::Ui, settings: &mut Settings, running: bool) {
        if !self.loaded {
            self.loaded = true;
            if let GearSource::Custom(custom) = &settings.gears {
                self.edit(custom);
            }
        }
        let selected = settings.drivetrain();
        if running {
            ui.label(RichText::new("Changes apply from the next ride.").color(WARN));
        }
        match &selected {
            Ok((name, d)) => {
                ui.label(
                    RichText::new(format!("Selected: {name}"))
                        .size(18.0)
                        .strong()
                        .color(ACCENT),
                );
                ui.label(
                    RichText::new(format!(
                        "{}, wheel {} mm",
                        bike_gearing::gear_count(d.chainrings.len() * d.cassette.len()),
                        d.wheel_circumference_mm
                    ))
                    .color(MUTED),
                );
            }
            Err(e) => {
                ui.label(RichText::new(format!("Selected setup is invalid: {e}")).color(WARN));
            }
        }
        ui.separator();

        if !self.loaded_view {
            self.loaded_view = true;
            if !settings.favorites.is_empty() {
                self.view = View::Favorites;
            }
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Templates").strong());
            ui.selectable_value(
                &mut self.view,
                View::Favorites,
                format!("★ Favorites ({})", settings.favorites.len()),
            );
            for category in Category::ALL {
                ui.selectable_value(&mut self.view, View::Category(category), category.label());
            }
            ui.selectable_value(&mut self.view, View::All, "All");
        });
        self.filters(ui);
        self.filter.category = match self.view {
            View::Category(c) => Some(c),
            View::Favorites | View::All => None,
        };
        let shown: Vec<&Template> = TEMPLATES
            .iter()
            .filter(|t| t.matches(&self.filter))
            .filter(|t| self.view != View::Favorites || settings.is_favorite(t.id))
            .collect();
        egui::ScrollArea::vertical()
            .id_salt("templates")
            .max_height(220.0)
            .show(ui, |ui| {
                if shown.is_empty() {
                    let hint = if self.view == View::Favorites && settings.favorites.is_empty() {
                        "No favorites yet: click ☆ next to a template to add it here."
                    } else {
                        "No template matches these filters."
                    };
                    ui.label(RichText::new(hint).color(MUTED));
                }
                for t in shown {
                    ui.horizontal(|ui| {
                        let star = settings.is_favorite(t.id);
                        let icon = RichText::new(if star { "★" } else { "☆" }).color(if star {
                            WARN
                        } else {
                            MUTED
                        });
                        if ui
                            .add(egui::Button::new(icon).frame(false))
                            .on_hover_text(if star {
                                "Remove from favorites"
                            } else {
                                "Add to favorites"
                            })
                            .clicked()
                        {
                            settings.toggle_favorite(t.id);
                        }
                        let current = settings.gears == GearSource::Template(t.id.into());
                        let label = format!(
                            "{}   ({})",
                            t.name,
                            bike_gearing::gear_count(t.gear_count())
                        );
                        if ui.selectable_label(current, label).clicked() {
                            settings.gears = GearSource::Template(t.id.into());
                        }
                    });
                }
            });

        if !settings.my_setups.is_empty() {
            ui.separator();
            ui.label(RichText::new("My setups").strong());
            self.my_setups(ui, settings);
        }

        ui.separator();
        ui.label(RichText::new("Custom setup").strong());
        egui::Grid::new("custom").num_columns(2).show(ui, |ui| {
            ui.label("Name");
            ui.add(
                egui::TextEdit::singleline(&mut self.name)
                    .hint_text("e.g. Winter gravel")
                    .desired_width(220.0),
            );
            ui.end_row();
            ui.label("Chainrings");
            ui.add(
                egui::TextEdit::singleline(&mut self.front)
                    .hint_text("50/34")
                    .desired_width(220.0),
            );
            ui.end_row();
            ui.label("Cassette");
            ui.add(
                egui::TextEdit::singleline(&mut self.rear)
                    .hint_text("11-12-13-...-28")
                    .desired_width(320.0),
            );
            ui.end_row();
            ui.label("Wheel");
            ui.add(
                egui::DragValue::new(&mut self.wheel_mm)
                    .range(1500..=2500)
                    .suffix(" mm"),
            );
            ui.end_row();
        });
        ui.horizontal(|ui| {
            if ui.button("Use custom setup").clicked()
                && let Some(custom) = self.parse()
            {
                settings.gears = GearSource::Custom(custom);
            }
            let named = !self.name.trim().is_empty();
            if ui
                .add_enabled(named, egui::Button::new("Save to My setups"))
                .on_disabled_hover_text("Give the setup a name first")
                .clicked()
                && let Some(custom) = self.parse()
            {
                settings.save_setup(custom.clone());
                settings.gears = GearSource::Custom(custom);
            }
        });
        ui.label(
            RichText::new("Typical wheels: 700×25C 2105 mm, 700×28C 2136 mm, 700×40C 2200 mm")
                .color(MUTED),
        );
        if let Some(e) = &self.error {
            ui.label(RichText::new(e).color(WARN));
        }

        if let Ok((_, d)) = &selected {
            ui.separator();
            ui.label(RichText::new("Calculator").strong());
            calculator(ui, d);
        }
    }
}

impl GearsTab {
    /// Brand, front, speeds and text filters, with a reset.
    fn filters(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let f = &mut self.filter;
            egui::ComboBox::from_id_salt("brand")
                .selected_text(f.brand.map_or("Any brand", Brand::label))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut f.brand, None, "Any brand");
                    for b in Brand::ALL {
                        ui.selectable_value(&mut f.brand, Some(b), b.label());
                    }
                });
            let front = |n: Option<usize>| match n {
                None => "1× or 2×".to_string(),
                Some(n) => format!("{n}× only"),
            };
            egui::ComboBox::from_id_salt("front")
                .selected_text(front(f.chainrings))
                .show_ui(ui, |ui| {
                    for n in [None, Some(1), Some(2)] {
                        ui.selectable_value(&mut f.chainrings, n, front(n));
                    }
                });
            let speeds = |n: Option<usize>| match n {
                None => "Any speed".to_string(),
                Some(n) => format!("{n}-speed"),
            };
            let mut available: Vec<usize> = TEMPLATES
                .iter()
                .map(|t| t.cassette.len())
                .filter(|n| (7..=13).contains(n))
                .collect();
            available.sort_unstable();
            available.dedup();
            egui::ComboBox::from_id_salt("speeds")
                .selected_text(speeds(f.speeds))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut f.speeds, None, speeds(None));
                    for n in available.into_iter().rev() {
                        ui.selectable_value(&mut f.speeds, Some(n), speeds(Some(n)));
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut f.text)
                    .hint_text("Search, e.g. GRX")
                    .desired_width(130.0),
            );
            let category = f.category;
            let active = *f
                != Filter {
                    category,
                    ..Filter::default()
                };
            if ui.add_enabled(active, egui::Button::new("Clear")).clicked() {
                *f = Filter::default();
            }
        });
    }

    fn my_setups(&mut self, ui: &mut egui::Ui, settings: &mut Settings) {
        let mut delete = None;
        for (i, setup) in settings.my_setups.iter().enumerate() {
            ui.horizontal(|ui| {
                let current = settings.gears == GearSource::Custom(setup.clone());
                let gears = setup.chainrings.len() * setup.cassette.len();
                let label = format!(
                    "{}   ({})",
                    setup.display_name(),
                    bike_gearing::gear_count(gears)
                );
                if ui.selectable_label(current, label).clicked() {
                    settings.gears = GearSource::Custom(setup.clone());
                }
                if ui.small_button("Edit").clicked() {
                    self.edit(setup);
                }
                if ui.small_button("Delete").clicked() {
                    delete = Some(i);
                }
            });
        }
        if let Some(i) = delete {
            settings.my_setups.remove(i);
        }
    }

    fn edit(&mut self, custom: &CustomGears) {
        self.name = custom.name.clone();
        self.front = join(&custom.chainrings, "/");
        self.rear = join(&custom.cassette, "-");
        self.wheel_mm = custom.wheel_mm;
        self.error = None;
    }

    /// The setup in the fields, or `None` with `self.error` explaining why.
    fn parse(&mut self) -> Option<CustomGears> {
        self.error = None;
        let (Some(chainrings), Some(mut cassette)) =
            (parse_teeth(&self.front), parse_teeth(&self.rear))
        else {
            self.error = Some("Use numbers separated by / or -, e.g. 50/34 and 11-12-13".into());
            return None;
        };
        cassette.sort_unstable();
        let custom = CustomGears {
            name: self.name.trim().into(),
            chainrings,
            cassette,
            wheel_mm: self.wheel_mm,
        };
        match custom.drivetrain() {
            Ok(_) => Some(custom),
            Err(e) => {
                self.error = Some(e.to_string());
                None
            }
        }
    }
}

fn join(teeth: &[u16], separator: &str) -> String {
    teeth
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(separator)
}

fn calculator(ui: &mut egui::Ui, d: &Drivetrain) {
    let gears = d.gears();
    egui::Grid::new("calculator")
        .striped(true)
        .num_columns(7)
        .spacing([18.0, 4.0])
        .show(ui, |ui| {
            for h in [
                "Gear",
                "Teeth",
                "Ratio",
                "m / rev",
                "km/h @ 60",
                "km/h @ 90",
                "Step",
            ] {
                ui.label(RichText::new(h).color(MUTED));
            }
            ui.end_row();
            for (i, g) in gears.iter().enumerate() {
                let start = g.number == d.default_gear();
                let number = RichText::new(g.number.to_string());
                ui.label(if start {
                    number.color(ACCENT).strong()
                } else {
                    number
                });
                ui.label(format!("{}×{}", g.chainring, g.cog));
                ui.label(format!("{:.2}", g.ratio));
                ui.label(format!("{:.2}", d.development_m(g)));
                ui.label(format!("{:.1}", d.speed_kmh(g, 60.0)));
                ui.label(format!("{:.1}", d.speed_kmh(g, 90.0)));
                ui.label(i.checked_sub(1).map_or(String::new(), |p| {
                    format!("{:+.1}%", step_percent(&gears[p], g))
                }));
                ui.end_row();
            }
        });
    ui.label(RichText::new("The highlighted gear is where each ride starts.").color(MUTED));
}
