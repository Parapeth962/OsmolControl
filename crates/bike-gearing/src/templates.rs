//! Ready-made setups. Tooth sequences follow the manufacturers' published cassette specs;
//! they were transcribed by hand, so check the spec sheet before relying on a single cog.
use crate::{Category, Drivetrain, GearError};
use serde::Serialize;

/// 700×28C road tyre and 700×40C gravel tyre, from common circumference tables.
const ROAD_WHEEL_MM: u16 = 2136;
const GRAVEL_WHEEL_MM: u16 = 2200;

// Shimano 7-speed (Tourney): MF-TZ500 freewheel 14-28, CS-HG200-7 12-28.
const S7_14_28: &[u16] = &[14, 16, 18, 20, 22, 24, 28];
const S7_12_28: &[u16] = &[12, 14, 16, 18, 21, 24, 28];
// Shimano 8-speed (Claris): CS-HG50-8.
const S8_11_30: &[u16] = &[11, 13, 15, 17, 20, 23, 26, 30];
const S8_11_32: &[u16] = &[11, 13, 15, 18, 21, 24, 28, 32];
const S8_11_34: &[u16] = &[11, 13, 15, 18, 21, 24, 28, 34];
// Shimano 9-speed (Sora): CS-HG400-9.
const S9_11_32: &[u16] = &[11, 12, 14, 16, 18, 21, 24, 28, 32];
const S9_11_34: &[u16] = &[11, 13, 15, 17, 20, 23, 26, 30, 34];
// Shimano 10-speed (Tiagra, GRX RX400): CS-4700, CS-HG500-10, CS-HG50-10.
const S10_11_28: &[u16] = &[11, 12, 13, 14, 15, 17, 19, 21, 24, 28];
const S10_11_32: &[u16] = &[11, 12, 14, 16, 18, 20, 22, 25, 28, 32];
const S10_11_34: &[u16] = &[11, 13, 15, 17, 19, 21, 23, 26, 30, 34];
const S10_11_36: &[u16] = &[11, 13, 15, 17, 19, 21, 24, 28, 32, 36];
// Shimano 11-speed (105 R7000, Ultegra R8000, GRX RX600/RX810): CS-R7000, CS-HG700-11.
const S11_11_28: &[u16] = &[11, 12, 13, 14, 15, 17, 19, 21, 23, 25, 28];
const S11_11_30: &[u16] = &[11, 12, 13, 14, 15, 17, 19, 21, 24, 27, 30];
const S11_11_32: &[u16] = &[11, 12, 13, 14, 16, 18, 20, 22, 25, 28, 32];
const S11_11_34: &[u16] = &[11, 13, 15, 17, 19, 21, 23, 25, 27, 30, 34];
// Shimano 12-speed (105 R7100, GRX RX820): CS-R7100, CS-HG710-12.
const S12_11_34: &[u16] = &[11, 12, 13, 14, 15, 17, 19, 21, 24, 27, 30, 34];
const S12_11_36: &[u16] = &[11, 12, 13, 14, 15, 17, 19, 21, 24, 27, 31, 36];
// SRAM 12-speed AXS (Rival/Force, XPLR).
const X12_10_33: &[u16] = &[10, 11, 12, 13, 14, 15, 17, 19, 21, 24, 28, 33];
const X12_10_36: &[u16] = &[10, 11, 12, 13, 15, 17, 19, 21, 24, 28, 32, 36];
const X12_10_44: &[u16] = &[10, 11, 13, 15, 17, 19, 21, 24, 28, 32, 38, 44];
const X12_10_28: &[u16] = &[10, 11, 12, 13, 14, 15, 16, 17, 19, 21, 24, 28];
const X12_10_30: &[u16] = &[10, 11, 12, 13, 14, 15, 17, 19, 21, 24, 27, 30];
// SRAM 12-speed Eagle 10-52 (GX/X01), the "mullet" gravel cassette.
const EAGLE_10_52: &[u16] = &[10, 12, 14, 16, 18, 21, 24, 28, 32, 36, 42, 52];
// SRAM 11-speed mechanical (Apex, Rival 22, Force 22): PG-1130.
const X11_11_28: &[u16] = &[11, 12, 13, 14, 15, 17, 19, 21, 23, 25, 28];
const X11_11_32: &[u16] = &[11, 12, 13, 14, 16, 18, 20, 22, 25, 28, 32];
const X11_11_36: &[u16] = &[11, 12, 13, 15, 17, 19, 22, 25, 28, 32, 36];
const X11_11_42: &[u16] = &[11, 13, 15, 17, 19, 22, 25, 28, 32, 36, 42];
// Campagnolo 12-speed (Chorus, Record, Super Record 12).
const C12_11_29: &[u16] = &[11, 12, 13, 14, 15, 16, 17, 19, 21, 23, 26, 29];
const C12_11_32: &[u16] = &[11, 12, 13, 14, 15, 17, 19, 21, 23, 26, 29, 32];
const C12_11_34: &[u16] = &[11, 12, 13, 14, 15, 17, 19, 22, 25, 28, 31, 34];
// Campagnolo 11-speed (Centaur, Potenza, Chorus 11).
const C11_12_27: &[u16] = &[12, 13, 14, 15, 16, 17, 19, 21, 23, 25, 27];
const C11_11_29: &[u16] = &[11, 12, 13, 14, 15, 17, 19, 21, 23, 26, 29];
const C11_11_32: &[u16] = &[11, 12, 13, 14, 15, 17, 19, 22, 25, 28, 32];
// Microshift Advent X 10-speed: CS-G104/CS-H104.
const MS10_11_48: &[u16] = &[11, 13, 15, 18, 21, 24, 28, 33, 39, 48];
// 13-speed. Campagnolo Ekar (BikeRadar review, cross-checked with two public gear datasets).
const EKAR_9_36: &[u16] = &[9, 10, 11, 12, 13, 14, 16, 18, 20, 23, 27, 31, 36];
const EKAR_9_42: &[u16] = &[9, 10, 11, 12, 13, 14, 16, 18, 21, 25, 30, 36, 42];
const EKAR_10_44: &[u16] = &[10, 11, 12, 13, 14, 15, 17, 19, 22, 26, 32, 39, 44];
// SRAM Red XPLR AXS 13-speed, XG-1391 10-46 (three public gear datasets agree).
const XPLR13_10_46: &[u16] = &[10, 11, 12, 13, 15, 17, 19, 21, 24, 28, 32, 38, 46];
// Campagnolo Super Record 13 road. Ranges and chainrings confirmed (BikeRadar); the cogs in
// between come from one public dataset only, so check them against the spec sheet.
const SR13_10_29: &[u16] = &[10, 11, 12, 13, 14, 15, 16, 17, 18, 20, 23, 26, 29];
const SR13_10_33: &[u16] = &[10, 11, 12, 13, 14, 15, 16, 18, 20, 23, 26, 29, 33];
const SR13_11_32: &[u16] = &[11, 12, 13, 14, 15, 16, 17, 18, 20, 23, 26, 29, 32];
const SR13_11_36: &[u16] = &[11, 12, 13, 14, 15, 16, 18, 20, 23, 26, 29, 32, 36];

// Very wide virtual ladders: one virtual chainring, cogs rising geometrically so every step
// is a similar few percent. Not hardware; generated, see the step range in each comment.
// 30 gears, ratio 4.46-0.85, steps 4.4-7.1 %.
const WIDE30_RING: &[u16] = &[125];
const WIDE30: &[u16] = &[
    28, 30, 32, 34, 36, 38, 40, 42, 45, 47, 50, 53, 56, 59, 63, 66, 70, 74, 79, 83, 88, 93, 99,
    105, 111, 117, 124, 131, 139, 147,
];
// 35 gears, ratio 4.34-0.85, steps 2.9-6.7 %.
const WIDE35_RING: &[u16] = &[126];
const WIDE35: &[u16] = &[
    29, 30, 32, 33, 35, 36, 38, 40, 42, 44, 46, 49, 51, 54, 56, 59, 62, 65, 68, 72, 75, 79, 83, 87,
    91, 96, 101, 106, 111, 116, 122, 128, 135, 141, 148,
];
// 40 gears, ratio 4.38-0.85, steps 2.6-6.5 %.
const WIDE40_RING: &[u16] = &[127];
const WIDE40: &[u16] = &[
    29, 30, 31, 33, 34, 36, 37, 39, 40, 42, 44, 46, 48, 50, 52, 54, 57, 59, 62, 64, 67, 70, 73, 76,
    79, 83, 86, 90, 94, 98, 102, 107, 111, 116, 121, 126, 132, 137, 143, 149,
];
// 45 gears, ratio 4.50-0.85, steps 2.1-6.1 %.
const WIDE45_RING: &[u16] = &[90];
const WIDE45: &[u16] = &[
    20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 35, 36, 37, 39, 40, 42, 43, 45, 47, 48,
    50, 52, 54, 56, 58, 60, 63, 65, 68, 70, 73, 76, 79, 82, 85, 88, 91, 95, 98, 102, 106,
];
// 50 gears, ratio 4.48-0.85, steps 2.0-5.4 %.
const WIDE50_RING: &[u16] = &[103];
const WIDE50: &[u16] = &[
    23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 39, 40, 41, 43, 44, 46, 47, 49, 51,
    52, 54, 56, 58, 60, 62, 64, 66, 69, 71, 73, 76, 78, 81, 84, 87, 90, 93, 96, 99, 102, 106, 110,
    113, 117, 121,
];

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Template {
    pub id: &'static str,
    pub category: Category,
    pub name: &'static str,
    pub chainrings: &'static [u16],
    pub cassette: &'static [u16],
    pub wheel_circumference_mm: u16,
}

impl Template {
    pub fn drivetrain(&self) -> Result<Drivetrain, GearError> {
        Drivetrain::new(self.chainrings, self.cassette, self.wheel_circumference_mm)
    }

    /// Manufacturer, from the id prefix every template follows.
    pub fn brand(&self) -> Brand {
        const SHIMANO: &[&str] = &["105-", "tiagra-", "sora-", "claris-", "tourney-", "grx-"];
        if SHIMANO.iter().any(|p| self.id.starts_with(p)) {
            Brand::Shimano
        } else if self.id.starts_with("sram-") {
            Brand::Sram
        } else if self.id.starts_with("campa-") {
            Brand::Campagnolo
        } else if self.id.starts_with("microshift-") {
            Brand::Microshift
        } else {
            Brand::Generic
        }
    }

    pub fn gear_count(&self) -> usize {
        self.chainrings.len() * self.cassette.len()
    }

    /// Whether the template passes `filter`.
    pub fn matches(&self, filter: &Filter) -> bool {
        filter.category.is_none_or(|c| c == self.category)
            && filter.brand.is_none_or(|b| b == self.brand())
            && filter.chainrings.is_none_or(|n| n == self.chainrings.len())
            && filter.speeds.is_none_or(|n| n == self.cassette.len())
            && (filter.text.trim().is_empty()
                || self
                    .name
                    .to_lowercase()
                    .contains(&filter.text.trim().to_lowercase()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Brand {
    Shimano,
    Sram,
    Campagnolo,
    Microshift,
    /// Generic ratios, single speeds and virtual ladders.
    Generic,
}

impl Brand {
    pub const ALL: [Self; 5] = [
        Self::Shimano,
        Self::Sram,
        Self::Campagnolo,
        Self::Microshift,
        Self::Generic,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Shimano => "Shimano",
            Self::Sram => "SRAM",
            Self::Campagnolo => "Campagnolo",
            Self::Microshift => "Microshift",
            Self::Generic => "Generic",
        }
    }
}

/// Template search: each `None` means "any"; `text` matches the name, case-insensitively.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filter {
    pub category: Option<Category>,
    pub brand: Option<Brand>,
    /// 1 for 1×, 2 for 2×.
    pub chainrings: Option<usize>,
    /// Cogs on the cassette (13 for 13-speed).
    pub speeds: Option<usize>,
    pub text: String,
}

const fn road(
    id: &'static str,
    name: &'static str,
    front: &'static [u16],
    rear: &'static [u16],
) -> Template {
    Template {
        id,
        category: Category::Road,
        name,
        chainrings: front,
        cassette: rear,
        wheel_circumference_mm: ROAD_WHEEL_MM,
    }
}
const fn special(
    id: &'static str,
    name: &'static str,
    front: &'static [u16],
    rear: &'static [u16],
) -> Template {
    Template {
        id,
        category: Category::Special,
        name,
        chainrings: front,
        cassette: rear,
        wheel_circumference_mm: ROAD_WHEEL_MM,
    }
}
const fn gravel(
    id: &'static str,
    name: &'static str,
    front: &'static [u16],
    rear: &'static [u16],
) -> Template {
    Template {
        id,
        category: Category::Gravel,
        name,
        chainrings: front,
        cassette: rear,
        wheel_circumference_mm: GRAVEL_WHEEL_MM,
    }
}

/// The first entry is the default for users who change nothing.
pub const TEMPLATES: &[Template] = &[
    road(
        "road-52-36-11-34",
        "Road 52/36 × 11-34 (11-speed)",
        &[52, 36],
        S11_11_34,
    ),
    // Shimano 105 R7000, 2×11.
    road(
        "105-r7000-50-34-11-28",
        "Shimano 105 R7000 2×11, 50/34 × 11-28",
        &[50, 34],
        S11_11_28,
    ),
    road(
        "105-r7000-50-34-11-30",
        "Shimano 105 R7000 2×11, 50/34 × 11-30",
        &[50, 34],
        S11_11_30,
    ),
    road(
        "105-r7000-50-34-11-32",
        "Shimano 105 R7000 2×11, 50/34 × 11-32",
        &[50, 34],
        S11_11_32,
    ),
    road(
        "105-r7000-50-34-11-34",
        "Shimano 105 R7000 2×11, 50/34 × 11-34",
        &[50, 34],
        S11_11_34,
    ),
    road(
        "105-r7000-52-36-11-28",
        "Shimano 105 R7000 2×11, 52/36 × 11-28",
        &[52, 36],
        S11_11_28,
    ),
    road(
        "105-r7000-52-36-11-30",
        "Shimano 105 R7000 2×11, 52/36 × 11-30",
        &[52, 36],
        S11_11_30,
    ),
    road(
        "105-r7000-52-36-11-32",
        "Shimano 105 R7000 2×11, 52/36 × 11-32",
        &[52, 36],
        S11_11_32,
    ),
    road(
        "105-r7000-53-39-11-28",
        "Shimano 105 R7000 2×11, 53/39 × 11-28",
        &[53, 39],
        S11_11_28,
    ),
    // Shimano 105 R7100, 2×12.
    road(
        "105-r7100-50-34-11-34",
        "Shimano 105 R7100 2×12, 50/34 × 11-34",
        &[50, 34],
        S12_11_34,
    ),
    road(
        "105-r7100-52-36-11-34",
        "Shimano 105 R7100 2×12, 52/36 × 11-34",
        &[52, 36],
        S12_11_34,
    ),
    road(
        "105-r7100-50-34-11-36",
        "Shimano 105 R7100 2×12, 50/34 × 11-36",
        &[50, 34],
        S12_11_36,
    ),
    // Shimano Tiagra 4700, 2×10.
    road(
        "tiagra-4700-50-34-11-28",
        "Shimano Tiagra 4700 2×10, 50/34 × 11-28",
        &[50, 34],
        S10_11_28,
    ),
    road(
        "tiagra-4700-50-34-11-32",
        "Shimano Tiagra 4700 2×10, 50/34 × 11-32",
        &[50, 34],
        S10_11_32,
    ),
    road(
        "tiagra-4700-50-34-11-34",
        "Shimano Tiagra 4700 2×10, 50/34 × 11-34",
        &[50, 34],
        S10_11_34,
    ),
    // Shimano Sora R3000, 2×9.
    road(
        "sora-r3000-50-34-11-32",
        "Shimano Sora R3000 2×9, 50/34 × 11-32",
        &[50, 34],
        S9_11_32,
    ),
    road(
        "sora-r3000-50-34-11-34",
        "Shimano Sora R3000 2×9, 50/34 × 11-34",
        &[50, 34],
        S9_11_34,
    ),
    // Shimano Claris R2000, 2×8.
    road(
        "claris-r2000-50-34-11-30",
        "Shimano Claris R2000 2×8, 50/34 × 11-30",
        &[50, 34],
        S8_11_30,
    ),
    road(
        "claris-r2000-50-34-11-32",
        "Shimano Claris R2000 2×8, 50/34 × 11-32",
        &[50, 34],
        S8_11_32,
    ),
    road(
        "claris-r2000-50-34-11-34",
        "Shimano Claris R2000 2×8, 50/34 × 11-34",
        &[50, 34],
        S8_11_34,
    ),
    // Shimano Tourney (A070 class), 2×7. Claris is 8-speed; entry 2×7 road bikes use Tourney.
    road(
        "tourney-2x7-50-34-14-28",
        "Shimano Tourney 2×7, 50/34 × 14-28",
        &[50, 34],
        S7_14_28,
    ),
    road(
        "tourney-2x7-50-34-12-28",
        "Shimano Tourney 2×7, 50/34 × 12-28",
        &[50, 34],
        S7_12_28,
    ),
    // SRAM AXS road, 2×12.
    road(
        "sram-axs-48-35-10-33",
        "SRAM Rival/Force AXS 2×12, 48/35 × 10-33",
        &[48, 35],
        X12_10_33,
    ),
    road(
        "sram-axs-46-33-10-36",
        "SRAM Rival/Force AXS 2×12, 46/33 × 10-36",
        &[46, 33],
        X12_10_36,
    ),
    // Gravel.
    gravel(
        "gravel-46-32-11-36",
        "Gravel 46/32 × 11-36 (10-speed)",
        &[46, 32],
        S10_11_36,
    ),
    gravel(
        "grx-rx400-46-30-11-36",
        "Shimano GRX RX400 2×10, 46/30 × 11-36",
        &[46, 30],
        S10_11_36,
    ),
    gravel(
        "grx-rx400-46-30-11-34",
        "Shimano GRX RX400 2×10, 46/30 × 11-34",
        &[46, 30],
        S10_11_34,
    ),
    gravel(
        "grx-rx400-46-30-11-32",
        "Shimano GRX RX400 2×10, 46/30 × 11-32",
        &[46, 30],
        S10_11_32,
    ),
    gravel(
        "grx-rx600-46-30-11-34",
        "Shimano GRX RX600/RX810 2×11, 46/30 × 11-34",
        &[46, 30],
        S11_11_34,
    ),
    gravel(
        "grx-rx810-48-31-11-34",
        "Shimano GRX RX810 2×11, 48/31 × 11-34",
        &[48, 31],
        S11_11_34,
    ),
    gravel(
        "grx-rx820-48-31-11-34",
        "Shimano GRX RX820 2×12, 48/31 × 11-34",
        &[48, 31],
        S12_11_34,
    ),
    gravel(
        "grx-rx820-48-31-11-36",
        "Shimano GRX RX820 2×12, 48/31 × 11-36",
        &[48, 31],
        S12_11_36,
    ),
    gravel(
        "claris-gravel-46-34-11-34",
        "Shimano Claris 2×8 gravel, 46/34 × 11-34",
        &[46, 34],
        S8_11_34,
    ),
    gravel(
        "sora-gravel-46-34-11-34",
        "Shimano Sora 2×9 gravel, 46/34 × 11-34",
        &[46, 34],
        S9_11_34,
    ),
    gravel(
        "sram-xplr-40-10-44",
        "SRAM XPLR 1×12, 40 × 10-44",
        &[40],
        X12_10_44,
    ),
    gravel(
        "sram-xplr-42-10-44",
        "SRAM XPLR 1×12, 42 × 10-44",
        &[42],
        X12_10_44,
    ),
    gravel(
        "sram-axs-43-30-10-36",
        "SRAM Rival AXS Wide 2×12, 43/30 × 10-36",
        &[43, 30],
        X12_10_36,
    ),
    // SRAM
    road(
        "sram-axs-50-37-10-28",
        "SRAM Red/Force AXS 2×12, 50/37 × 10-28",
        &[50, 37],
        X12_10_28,
    ),
    road(
        "sram-axs-48-35-10-30",
        "SRAM Rival/Force AXS 2×12, 48/35 × 10-30",
        &[48, 35],
        X12_10_30,
    ),
    road(
        "sram-11s-50-34-11-28",
        "SRAM Apex/Rival 22 2×11, 50/34 × 11-28",
        &[50, 34],
        X11_11_28,
    ),
    road(
        "sram-11s-50-34-11-32",
        "SRAM Apex/Rival 22 2×11, 50/34 × 11-32",
        &[50, 34],
        X11_11_32,
    ),
    road(
        "sram-11s-52-36-11-28",
        "SRAM Force 22 2×11, 52/36 × 11-28",
        &[52, 36],
        X11_11_28,
    ),
    gravel(
        "sram-11s-46-36-11-36",
        "SRAM Apex/Rival 2×11 gravel, 46/36 × 11-36",
        &[46, 36],
        X11_11_36,
    ),
    gravel(
        "sram-1x11-40-11-42",
        "SRAM Apex 1/Rival 1 1×11, 40 × 11-42",
        &[40],
        X11_11_42,
    ),
    gravel(
        "sram-1x11-42-11-42",
        "SRAM Apex 1/Rival 1 1×11, 42 × 11-42",
        &[42],
        X11_11_42,
    ),
    gravel(
        "sram-mullet-40-10-52",
        "SRAM mullet 1×12, 40 × 10-52 (Eagle)",
        &[40],
        EAGLE_10_52,
    ),
    // Campagnolo
    road(
        "campa-12s-50-34-11-29",
        "Campagnolo Chorus/Record 2×12, 50/34 × 11-29",
        &[50, 34],
        C12_11_29,
    ),
    road(
        "campa-12s-50-34-11-32",
        "Campagnolo Chorus/Record 2×12, 50/34 × 11-32",
        &[50, 34],
        C12_11_32,
    ),
    road(
        "campa-12s-52-36-11-29",
        "Campagnolo Record 2×12, 52/36 × 11-29",
        &[52, 36],
        C12_11_29,
    ),
    road(
        "campa-12s-48-32-11-34",
        "Campagnolo Chorus 2×12, 48/32 × 11-34",
        &[48, 32],
        C12_11_34,
    ),
    road(
        "campa-11s-50-34-11-29",
        "Campagnolo Centaur/Potenza 2×11, 50/34 × 11-29",
        &[50, 34],
        C11_11_29,
    ),
    road(
        "campa-11s-50-34-11-32",
        "Campagnolo Centaur/Potenza 2×11, 50/34 × 11-32",
        &[50, 34],
        C11_11_32,
    ),
    road(
        "campa-11s-53-39-12-27",
        "Campagnolo Chorus 2×11, 53/39 × 12-27",
        &[53, 39],
        C11_12_27,
    ),
    // 13-speed
    road(
        "campa-sr13-52-36-10-29",
        "Campagnolo Super Record 13 2×13, 52/36 × 10-29",
        &[52, 36],
        SR13_10_29,
    ),
    road(
        "campa-sr13-50-34-10-33",
        "Campagnolo Super Record 13 2×13, 50/34 × 10-33",
        &[50, 34],
        SR13_10_33,
    ),
    road(
        "campa-sr13-48-32-11-32",
        "Campagnolo Super Record 13 2×13, 48/32 × 11-32",
        &[48, 32],
        SR13_11_32,
    ),
    gravel(
        "campa-sr13-45-29-11-36",
        "Campagnolo Super Record 13 All Road 2×13, 45/29 × 11-36",
        &[45, 29],
        SR13_11_36,
    ),
    gravel(
        "campa-ekar-40-9-36",
        "Campagnolo Ekar 1×13, 40 × 9-36",
        &[40],
        EKAR_9_36,
    ),
    gravel(
        "campa-ekar-42-9-42",
        "Campagnolo Ekar 1×13, 42 × 9-42",
        &[42],
        EKAR_9_42,
    ),
    gravel(
        "campa-ekar-40-10-44",
        "Campagnolo Ekar 1×13, 40 × 10-44",
        &[40],
        EKAR_10_44,
    ),
    gravel(
        "campa-ekar-38-10-44",
        "Campagnolo Ekar 1×13, 38 × 10-44",
        &[38],
        EKAR_10_44,
    ),
    gravel(
        "sram-xplr13-42-10-46",
        "SRAM Red XPLR 1×13, 42 × 10-46",
        &[42],
        XPLR13_10_46,
    ),
    gravel(
        "sram-xplr13-46-10-46",
        "SRAM Red XPLR 1×13, 46 × 10-46",
        &[46],
        XPLR13_10_46,
    ),
    // Microshift
    gravel(
        "microshift-advent-x-40-11-48",
        "Microshift Advent X 1×10, 40 × 11-48",
        &[40],
        MS10_11_48,
    ),
    gravel(
        "microshift-advent-x-38-11-48",
        "Microshift Advent X 1×10, 38 × 11-48",
        &[38],
        MS10_11_48,
    ),
    // Single speed: popular track, fixie and single-speed gravel ratios.
    special(
        "single-48-17",
        "Single speed 48 × 17 (track, ratio 2.82)",
        &[48],
        &[17],
    ),
    special(
        "single-46-16",
        "Single speed 46 × 16 (fixie, ratio 2.88)",
        &[46],
        &[16],
    ),
    special(
        "single-44-16",
        "Single speed 44 × 16 (city, ratio 2.75)",
        &[44],
        &[16],
    ),
    special(
        "single-42-16",
        "Single speed 42 × 16 (gravel, ratio 2.63)",
        &[42],
        &[16],
    ),
    special(
        "single-42-18",
        "Single speed 42 × 18 (hilly, ratio 2.33)",
        &[42],
        &[18],
    ),
    // Very wide virtual ladders.
    special(
        "wide-30",
        "Wide virtual 30-speed (4–7 % steps)",
        WIDE30_RING,
        WIDE30,
    ),
    special(
        "wide-35",
        "Wide virtual 35-speed (3–7 % steps)",
        WIDE35_RING,
        WIDE35,
    ),
    special(
        "wide-40",
        "Wide virtual 40-speed (3–7 % steps)",
        WIDE40_RING,
        WIDE40,
    ),
    special(
        "wide-45",
        "Wide virtual 45-speed (2–6 % steps)",
        WIDE45_RING,
        WIDE45,
    ),
    special(
        "wide-50",
        "Wide virtual 50-speed (2–5 % steps)",
        WIDE50_RING,
        WIDE50,
    ),
];

pub fn template(id: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brands_and_filters() {
        let brand = |id| template(id).map(Template::brand);
        assert_eq!(brand("105-r7000-50-34-11-28"), Some(Brand::Shimano));
        assert_eq!(brand("grx-rx820-48-31-11-34"), Some(Brand::Shimano));
        assert_eq!(brand("sram-xplr13-42-10-46"), Some(Brand::Sram));
        assert_eq!(brand("campa-ekar-40-9-36"), Some(Brand::Campagnolo));
        assert_eq!(
            brand("microshift-advent-x-40-11-48"),
            Some(Brand::Microshift)
        );
        assert_eq!(brand("wide-50"), Some(Brand::Generic));
        let thirteen = Filter {
            speeds: Some(13),
            ..Filter::default()
        };
        let found: Vec<_> = TEMPLATES.iter().filter(|t| t.matches(&thirteen)).collect();
        assert!(found.len() >= 10);
        assert!(found.iter().all(|t| t.cassette.len() == 13));
        let one_by_campa = Filter {
            brand: Some(Brand::Campagnolo),
            chainrings: Some(1),
            text: "EKAR".into(),
            ..Filter::default()
        };
        assert_eq!(
            TEMPLATES
                .iter()
                .filter(|t| t.matches(&one_by_campa))
                .count(),
            4
        );
        assert!(TEMPLATES.iter().all(|t| t.matches(&Filter::default())));
    }

    #[test]
    fn wide_ladders_have_their_gear_count_and_even_steps() {
        for (id, count) in [("wide-30", 30), ("wide-40", 40), ("wide-50", 50)] {
            let Some(Ok(d)) = template(id).map(Template::drivetrain) else {
                panic!("{id} missing")
            };
            let gears = d.gears();
            assert_eq!(gears.len(), count, "{id}");
            let steps: Vec<f64> = gears
                .windows(2)
                .map(|w| crate::step_percent(&w[0], &w[1]))
                .collect();
            assert!(
                steps.iter().all(|s| (1.5..=7.5).contains(s)),
                "{id}: {steps:?}"
            );
        }
    }

    #[test]
    fn every_template_is_valid_unique_and_strictly_increasing() {
        let mut ids = std::collections::HashSet::new();
        for t in TEMPLATES {
            assert!(ids.insert(t.id), "duplicate id {}", t.id);
            assert!(t.drivetrain().is_ok(), "{}", t.id);
            assert!(
                t.cassette.windows(2).all(|w| w[0] < w[1]),
                "{} cassette order",
                t.id
            );
            assert!(
                t.chainrings.windows(2).all(|w| w[0] > w[1]),
                "{} chainring order",
                t.id
            );
        }
    }

    #[test]
    fn requested_gear_counts() {
        let count = |id| template(id).map(|t| t.chainrings.len() * t.cassette.len());
        assert_eq!(count("tourney-2x7-50-34-14-28"), Some(14));
        assert_eq!(count("grx-rx400-46-30-11-36"), Some(20));
        assert_eq!(count("claris-r2000-50-34-11-32"), Some(16));
        assert_eq!(count("105-r7000-50-34-11-28"), Some(22));
    }
}
