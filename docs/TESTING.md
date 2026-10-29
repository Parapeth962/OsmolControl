# Testing OsmolControl

Three levels: automated tests (run everywhere, also in CI), checks on a Linux desktop without
devices, and a ride with real hardware. Run them in this order; each level assumes the one
before passed.

## 1. Automated (`cargo test`)

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
packaging/linux/test-install.sh   # CI also runs bash -n and shellcheck on the scripts
```

What they cover, by crate:

| Area | Covered |
| --- | --- |
| `bike-protocol` | FTMS, Cycling Power, CSC and heart rate decoding from captured frames; OpenBikeControl framing; FTMS safety filter (only allowed commands, targets clamped, grade limited); trainer write queue |
| `bike-core` | Controller profiles (built-in and user), button decoding of every supported controller, learning a new controller, reconnect budget |
| `bike-gearing` | Gear lists and ordering, calculator figures, validation, every template; road physics, shifting, slew limit, grade scale |
| `bike-ble` | Controller and strap monitoring over a mock link: subscriptions, readiness, stale data, reconnects, timeouts, cancellation; trainer control replay |
| `bike-dircon` | DirCon codec, GATT discovery/read/write/notify, a real loopback TCP session, malformed peers |
| `bike-mywhoosh` | OpenBikeControl over real TCP: fragmentation, capabilities, stale and not-ready presses dropped, unknown messages |
| `bike-session` router | Simulation consumed and turned into power targets; ERG and reset pause the gears; targets within the trainer's limits and not repeated; own trainer replies hidden from the app; every Claris shift reaches the trainer |
| `bike-session` loop | `bridge::run` end to end over channels: telemetry relayed, road acknowledged, **a shift sent before the next tick**, late presses dropped, ERG passed through untouched, stop ends it |
| `bike-session` session | Heart rate strap searched only when enabled and with a trainer; a trainer that is switched off does not end the ride; settings (old files load, options round trip); status freshness; log bound |
| `bikectl` | Command-line definition, defaults and refused flag combinations; `start` flags to session settings |
| `bikecontrol` | Climb difficulty labels, status text, custom gear setup parsing and editing; every symbol in the interface exists in egui's fonts (no empty boxes) |
| Packaging | `install.sh` against a throwaway home with stand-ins for sudo and the package manager: fresh install, rerun to update (game kept, launcher refreshed), second edition, bad option; `shellcheck`; `ble-bridge.py` compiles |

Not covered automatically: real Bluetooth hardware, the egui windows themselves, the X11
overlay stacking (needs an X server and a window manager), installing real system packages.

## 2. Linux desktop, no devices needed

Build once: `cargo build --locked --release -p bikecontrol -p bikectl`.

1. **Every tab renders:** for each of `ride gears controllers settings log`, run
   `target/release/osmolcontrol --demo --tab <tab> --screenshot /tmp/<tab>.png`; it must exit 0
   with a non-empty PNG. Repeat without `--demo` (your saved settings) and with
   `XDG_DATA_HOME=<empty dir>` (first start: defaults, heart rate on).
2. **Display fallbacks:** the same with `env -u DISPLAY` (native Wayland) and `DISPLAY=:97`
   (no X server); both must start.
3. **Overlay above a fullscreen game:** start `osmolcontrol --demo --overlay`, make any window
   fullscreen and focused. The overlay must stay visible on top, follow the mouse when dragged
   (same distance as the pointer) and close with 🗙. Checked on KDE Plasma 6.8 and on GNOME's
   mutter 51 (headless, with a stand-in fullscreen window); other desktops are untested.
4. **Command line:** `bikectl gears`, `bikectl gears <id>`, a custom `--front/--rear`, and wrong
   input (unknown template, duplicate teeth, wheel 900 mm, `decode` with bad hex) must give a
   clear message and exit code 1 or 2, never a panic.
5. **Bluetooth, read only:** `bikectl adapters` and `bikectl scan --seconds 6`.
6. **Trainer switched off:** `bikectl start --trainer-bridge --gears claris-r2000-50-34-11-32
   --no-heart-rate --seconds 45` must keep logging "Cannot connect ... Still looking." and stop
   after 45 s, not end with an error.
7. **Packaging:** `mywhoosh.sh doctor` on an installed MyWhoosh must report every line ok.
8. **Command line output into a pipe:** `bikectl gears | head -1` must exit 0 without a panic.

## 3. Ride with hardware

Trainer, controller, optional heart rate strap, MyWhoosh. Tick each line.

**Start**
- [ ] Trainer off, press Start ride, then switch the trainer on: it is found without a restart.
- [ ] Controller found and shown with battery; Ride tab shows trainer, controller, MyWhoosh green.
- [ ] MyWhoosh pairs OsmolControl as Power Source; power and cadence match the trainer display.
- [ ] In *Virtual trainer, MyWhoosh shifting* or *Controller only*, MyWhoosh still lists
      OsmolControl as a controller (its LAN advertisement now says `model=OsmolControl`).

**Shifting (OsmolControl gears, e.g. Claris 2×8)**
- [ ] Each press changes resistance within about half a second, up and down.
- [ ] Gear 1 is the easiest, the top gear the hardest; holding a cadence, every press up is harder.
- [ ] Fast presses (several per second) are all applied; none arrive seconds later.
- [ ] At either end of the range, more presses do nothing.
- [ ] On a climb the resistance rises; Climb difficulty 50 % feels clearly easier than 100 %.
- [ ] During an ERG workout the power follows MyWhoosh; gears change nothing until it ends.

**Heart rate**
- [ ] *Use a heart rate strap* on: the strap is found, bpm on the Ride tab and in MyWhoosh.
- [ ] Off: log says "Heart rate sensor off in Settings."; no strap search, no heart rate shown.

**Overlay**
- [ ] Over MyWhoosh in fullscreen; gear, power, cadence, grade update; − / + shift.

**End**
- [ ] Stop releases the trainer (it freewheels); MyWhoosh shows it disconnected.
- [ ] Start again in the same app: everything reconnects, no doubled readings.
