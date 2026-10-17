# Command line and developer reference

The desktop app covers normal use; see the [manual](MANUAL.md). This page documents the `bikectl` command line tool, building from source, and the low-level diagnostics used while developing OsmolControl.

## Build

Install Rust via rustup and Linux D-Bus development headers (`libdbus-1-dev` and `pkg-config` on Debian/Ubuntu; `dbus` and `pkgconf` on Arch). The pinned toolchain is installed by rustup when needed.

```sh
cargo build --locked --release -p bikectl
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
```

The executable is `target/release/bikectl`. Run it as your ordinary desktop user with access to BlueZ/system D-Bus. For a local install use `cargo install --locked --path apps/bikectl`.

## Discover and read devices

```sh
bikectl adapters
bikectl scan --seconds 10
bikectl inspect '<exact ID or exact advertised name>'
bikectl monitor '<trainer ID>' --seconds 60
bikectl monitor '<trainer ID>' '<second device ID>' --seconds 60
```

Use IDs returned by the current platform; macOS IDs are not MAC addresses. Select a different adapter with `--adapter 1`. Scan output includes cached peripherals: seeing a device does not prove it is currently reachable. An ambiguous name is rejected. Inspection connects, enumerates GATT and reads readable characteristics, then disconnects. Monitoring subscribes only to supported standard measurements/status and any explicitly configured vendor characteristic. It never requests trainer control or writes resistance targets.

Output is JSON/JSON-lines on stdout, diagnostic logs on stderr (`RUST_LOG=debug` for more detail). Events carry timestamps, characteristic UUIDs, raw bytes and decoded values. Missing fields stay absent. Each device reconnects independently with bounded backoff; exhausted retries produce an error. A quiet stream is marked stale after five seconds. Ctrl-C or `--seconds` ends monitoring and attempts clean disconnect.

## Identify the V100 protocol

First inspect the actual device. Do not assume it uses the VS200 protocol.

```sh
bikectl capture '<V100 ID>' '<notifiable characteristic UUID>' --seconds 60
```

Capture is raw and cannot trigger actions. Label press/release/hold/repeat sequences in notes. If evidence proves that a single exact packet represents one complete click, create a local JSON mapping:

```json
{
  "characteristic": "<UUID from the real device>",
  "shift_up_hex": "<captured complete click packet>",
  "shift_down_hex": "<captured complete click packet>",
  "evidence": "local capture filename and labeled event timestamps"
}
```

These placeholders deliberately are not a guessed V100 configuration. Unknown packets do nothing. Each match produces one press/release pair; protocols with separate states, counters or repeats need a dedicated decoder after capture analysis. Test rapid distinct presses, repeated notifications and button holds before calling the mapping validated.

```sh
bikectl monitor '<V100 ID>' --mapping v100.local.json --seconds 60
bikectl monitor '<trainer ID>' '<V100 ID>' --controller '<V100 ID>' --mapping v100.local.json
```

## MyWhoosh connection

Run MyWhoosh on a supported host on the same LAN. On Linux find the LAN IPv4 with `ip -4 -brief address`; select the physical network, not Docker/VPN. Start the controller server:

```sh
bikectl connect-mywhoosh --bind 192.168.1.10 --stdin-actions
```

Replace the example IP with your machine's address. In MyWhoosh's connection screen, use the OpenBikeControl control and select OsmolControl. The server advertises `_openbikecontrol._tcp.local.` and listens on TCP 36867 by default (`--port` changes it). Type `up` or `down` then Enter for a deliberate manual action test. Use `--seconds 60` for a bounded session. There is no remote shell/HTTP UI.

Once V100 packets are verified:

```sh
bikectl connect-mywhoosh --bind 192.168.1.10 \
  --controller '<V100 ID>' --mapping v100.local.json
```

Add `--trainer '<trainer ID>'` only to read trainer telemetry in the same process. **The OpenBikeControl link carries controller actions, not trainer power/cadence/resistance.** MyWhoosh must pair with the trainer separately. In this low-level command MyWhoosh stays the only resistance/virtual-shifting owner; for the trainer bridge use `bikectl start --trainer-bridge` or the desktop app.

The server accepts up to four clients, honors optional app capabilities, drops stale queued actions and clears capability state on reconnect. Device-ready status follows the mapped BLE controller; manual stdin mode is ready while enabled. Haptic requests are parsed but not forwarded to unknown hardware. No app-info message means all actions are assumed supported, per specification. This implements the wire protocol; real MyWhoosh interoperability remains to be verified.

Allow multicast DNS UDP 5353 and the chosen TCP port on the trusted LAN. On a Windows MyWhoosh host, check Bonjour if discovery fails. `--no-mdns` is for direct-address diagnostics; do not assume MyWhoosh offers manual IP entry. The app does not modify your firewall or system services. See [network contract](protocol/mywhoosh.md).

## Offline decoding and diagnostics

```sh
bikectl decode 2ad2 4400c409b400fa00
bikectl decode 2a63 2000fa00ffff0004
```

Raw scan/inspection/capture output can contain device addresses, serials and manufacturer bytes; remove them before sharing. Local packet mapping files matching `*.local.json` are ignored by git.

Readable support depends on advertised GATT capabilities:

| Data | Support |
| --- | --- |
| Indoor Bike Data | Speed, cadence, power, resistance level, distance, energy, heart rate, time, optional fields |
| Cycling Power | Power, optional torque/force/angles/energy and revolution counters |
| CSC | Wheel/crank counters and event ticks; no invented speed/cadence from one sample |
| Features/ranges | FTMS/CPS/CSC raw bitsets; supported power/resistance ranges |
| Device Information / battery | Read when offered; missing values remain unavailable |
| Vendor/V100 | Raw capture or explicit verified packet mapping; no automatic V100 compatibility claim |

Firmware update, legacy MyWhoosh Link, BLE peripheral bridging and ride recording are not implemented. Trainer control is only used by `bikectl start --trainer-bridge` and the desktop app, through the safety filter.
