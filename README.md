# OsmolControl

**Virtual shifting with real gear setups for MyWhoosh.** OsmolControl connects your smart trainer
and your shift controller over Bluetooth, presents them to MyWhoosh as one trainer, and makes
the resistance follow the gear you choose: Shimano, SRAM, Campagnolo, Microshift, single speed,
or a 50-speed virtual ladder.

![OsmolControl ride screen](docs/images/ride.png)

**[User manual: install and ride, step by step →](docs/MANUAL.md)**

## Features

- **Any FTMS smart trainer.** Found automatically; power and slope limits are read from the
  trainer. No brand-specific code.
- **Shift controllers:** ThinkRider (V100, VS100, VS200), CYCPLUS BC2, Elite Square and
  Shimano Di2 built in; teach it others with a few button presses; plus on-screen buttons and
  keyboard shortcuts.
- **Gear templates** for road, gravel (including 1×13 and 2×13), single speed and very wide
  virtual ladders, with brand/type/speed filters, search, favorites, custom setups you can
  save, and a gear calculator.
- **Real road feel:** MyWhoosh's slope, your weight and your gear set the target power; ERG
  workouts pass straight through.
- **Heart rate straps:** any standard Bluetooth strap is passed on to MyWhoosh.
- **Always-on-top overlay** with gear, power, cadence, grade and heart rate.
- **Safety first:** every trainer command goes through a filter (allowed commands only, clamped
  to the trainer's own limits, rate limited), and the trainer is reset when you stop.
- **Windows, macOS and Linux.** On Linux, `packaging/linux/install.sh` also installs MyWhoosh
  itself under Proton, so everything runs on one PC.

## Install

| Platform | How |
| --- | --- |
| Windows | Unzip `OsmolControl-Windows.zip`, run `OsmolControl.exe` |
| macOS | Unpack `OsmolControl-macOS.tar.gz`, run `osmolcontrol` |
| Linux | `packaging/linux/install.sh` (apt, dnf or pacman) |

Details, screenshots and troubleshooting: **[docs/MANUAL.md](docs/MANUAL.md)**.

## Hardware support

- **Trainers:** anything that advertises the Bluetooth FTMS service (Wahoo, Elite, Tacx/Garmin,
  Saris, Zwift Hub, ThinkRider and others). Tested live with a ThinkRider X2 Max.
- **Controllers:** ThinkRider V100 (tested live), VS100 and VS200; CYCPLUS BC2, Elite Square and
  Shimano Di2 (D-Fly) from their published protocols. Controllers that send one fixed message
  per button press can be learned in the **Controllers** tab ([manual, section 7](docs/MANUAL.md#7-shift-controllers)).
  Zwift Click/Play/Ride and other encrypted or two-way protocols are not supported yet.
- **MyWhoosh:** 5.7 and 6.2 tested, over LAN (Windows/macOS/other devices) and over Bluetooth
  on the same Linux PC.

## For developers

```sh
cargo build --locked --release -p bikecontrol -p bikectl
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
```

Linux needs `libdbus-1-dev`/`dbus-devel` and `pkg-config`. The workspace:

| Crate | Role |
| --- | --- |
| `bike-protocol` | FTMS/CPS decoding, trainer command safety filter |
| `bike-core` | controller profiles, packet maps, retry policy |
| `bike-ble` | Bluetooth (btleplug): discovery, monitoring, the trainer link |
| `bike-gearing` | drivetrains, templates, road physics, virtual shifter |
| `bike-dircon` | DirCon (Wahoo Direct Connect) virtual trainer server |
| `bike-mywhoosh` | OpenBikeControl controller server |
| `bike-session` | the riding session shared by the GUI and the CLI |
| `apps/bikecontrol` | desktop app (`osmolcontrol`, egui) |
| `apps/bikectl` | command line tool |

Command line usage and diagnostics: [docs/cli.md](docs/cli.md). Protocol notes:
[docs/protocol](docs/protocol). Device notes: [docs/devices](docs/devices).

MyWhoosh is a trademark of its owners; this project is not affiliated with MyWhoosh.

Licensed under the [MIT License](LICENSE).
