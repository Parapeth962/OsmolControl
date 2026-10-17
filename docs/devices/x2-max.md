# ThinkRider X2 Max — current evidence

## 2026-10-17 live identification (observed)

Host: Linux, BlueZ 5.87, single adapter `hci0` (USB dongle, controller manufacturer 0x000a). Trainer powered from mains; previously bonded in BlueZ. Raw captures are not published.

- The device does not advertise while unpowered. Earlier `connect: timed out` results (2026-10-16) are explained by the trainer being off; no other central was involved.
- When powered it advertises as `Think X222- XXXXXXX` (random static address), RSSI about -41 dBm at close range, with FTMS (0x1826) service data in the advertisement.
- `bikectl inspect` connected and completed without pairing prompts. No writes were performed.

The tester identifies this unit as the X2 Max. The device itself reports the following; nothing in it names "X2 Max" explicitly:

| Characteristic | Raw | Meaning |
| --- | --- | --- |
| 2A00 Device Name | `Think X222- XXXXXXX` | advertised name; suffix looks like a unit serial (treat as an identifier) |
| 2A01 Appearance | `0000` | unknown/generic |
| 2A04 Preferred Connection Parameters | `2800500000009001` | interval 50–100 ms, latency 0, supervision timeout 4 s |
| 2A24 Model Number | `20250717` | ASCII; looks like a date, not a model name |
| 2A26 Firmware Revision | `V3.2.3` | |
| 2A27 Hardware Revision | `V1.1.0` | |
| 2A28 Software Revision | `V3.2.3` | |
| 2A29 Manufacturer | `ThinkRider` | |

No Battery Service (0x180F); the trainer is mains powered. Battery is reported as unavailable, not zero.

### GATT services

| Service | Characteristics (properties) |
| --- | --- |
| 1800 Generic Access | 2A00 (read/write), 2A01, 2A04, 2AA6 Central Address Resolution = 1 |
| 1801 Generic Attribute | none exposed |
| 180A Device Information | 2A24, 2A26, 2A27, 2A28, 2A29 (read) |
| 1818 Cycling Power | 2A5D Sensor Location = `0c` (rear wheel); 2A63 Measurement (notify); 2A65 Feature = `0e000000`; 2A66 Control Point (write/indicate) |
| 1826 Fitness Machine | 2ACC Feature = `034000000ca00000`; 2AD2 Indoor Bike Data (notify); 2AD6 Resistance Range = `0000c8000100`; 2AD8 Power Range = `0000d0070100`; 2AD9 Control Point (write/indicate); 2ADA Machine Status (notify) |
| FD00 (vendor) | FD09 notify, FD0A write-without-response, FD19 notify, FD1A write-without-response |
| 6e40fec1-b5a3-f393-e0a9-e50e24dcca9e (vendor) | 6e40fec2 notify, 6e40fec3 write/write-without-response |

There is no Cycling Speed and Cadence service (0x1816).

### Decoded capability values (standard specifications)

- **FTMS Fitness Machine Features `0x00004003`:** average speed (bit 0), cadence (bit 1), power measurement (bit 14).
- **FTMS Target Setting Features `0x0000A00C`:** resistance target (bit 2), power target / ERG (bit 3), indoor bike simulation parameters (bit 13), spin-down control (bit 15).
- **Supported resistance level range:** 0.0–20.0, increment 0.1.
- **Supported power range:** 0–2000 W, increment 1 W.
- **Cycling Power Features `0x0000000E`:** accumulated torque (bit 1), wheel revolution data (bit 2), crank revolution data (bit 3).

Advertised control capabilities are recorded only. No FTMS or Cycling Power control point command has been sent, and the vendor services have not been subscribed or written.

### Telemetry

Notification cadence while connected: Indoor Bike Data (2AD2) about 4 Hz, Cycling Power Measurement (2A63) about 2 Hz. At rest:

- 2AD2 `4400000000000000`: flags `0x0044`, so instantaneous speed, instantaneous cadence and instantaneous power are present; all are zero.
- 2A63 `3000000000000000000000000000`: flags `0x0030`, so wheel and crank revolution data are present; power and counters are zero.

Hand-spin capture (`monitor-x2-hand-spin.jsonl`, 90 s; drivetrain spun by hand, no reference display available):

| Phase (approx.) | FTMS speed | FTMS power | CP power | Cadence (both) |
| --- | --- | --- | --- | --- |
| 0–56 s spinning | 2–9.7 km/h | 0–28 W | tracks FTMS, about 2 Hz | 0 throughout |
| 56–62 s coasting | decays 4.7 → 0 km/h | drops to 0 by about 58 s | 0 | 0 |
| 62–90 s stopped | 0 | 0 | 0 | 0 |

- FTMS 2AD2 sample `4400ef0100000500` decodes per specification: speed 0x01EF × 0.01 = 4.95 km/h, cadence 0, power 5 W. The project decoder agrees.
- CP 2A63 wheel revolution count advanced 2 → 59, one revolution per changed update. The 1/2048 s wheel event time wrapped twice in 90 s (it wraps every 32 s), so speed derivation from CP must handle the wrap.
- Crank revolutions stayed 0 and instantaneous cadence stayed 0 in both services during hand spinning. Unresolved: either the trainer measures cadence only under real pedalling load, or cadence needs a separate sensor. Needs a capture while actually pedalling.
- Absolute power/speed accuracy is **unverified**: no trusted reference was available.
- In the later `vendor-fd09-idle.jsonl` capture, taken while the drivetrain was still turning, CP 2A63 reported **crank revolutions = 32** with a non-zero crank event time (constant over the capture). So the trainer does populate crank data in some conditions; when is unresolved.

### Vendor services (read-only subscription, idle, 20 s each)

Captures: `vendor-fd09-idle.jsonl`, `vendor-fd19-idle.jsonl`, `vendor-fec2-idle.jsonl`. Only the CCCD subscription was written; no characteristic writes.

- **FD00 (FD09, FD19):** no notifications while idle without any command. Purpose unknown.
- **6e40fec1 (FEC2):** about 1.6 notifications/s without any command. Every frame is 13 bytes in ANT serial-message framing: `A4` sync, length `09`, message ID `4E` (broadcast data), channel `05`, 8-byte payload, XOR checksum. All 33 frames passed the checksum. This matches the "FE-C over BLE" transport (*inferred*: framing and checksum observed; the page meanings come from the public ANT+ FE-C profile, not from device documentation).

| Frame | FE-C page | Decoded (per FE-C profile) |
| --- | --- | --- |
| `a4094e05 10 19 36 8c 0000 ff 14 be` | 16 General FE Data | equipment type 0x19 (trainer), elapsed time 0.25 s units incrementing 1 s/frame, distance 140 m, speed 0, HR invalid (0xFF), capabilities 0x4, FE state 1 (asleep/off) |
| `a4094e05 19 5b 00 6a9c 0000 10 42` | 25 Specific Trainer Data | event count 91, cadence 0 rpm, accumulated power 40042 W, instantaneous power 0, trainer status 0 (no calibration required), FE state 1 |

FE-C page 25 includes its own cadence field, so it is a second candidate cadence source alongside FTMS/CP. Control pages over 6e40fec3 (write) were not sent.

### Real pedalling capture (`ride2-x2.jsonl`, about 90 s, user riding at about 60 rpm)

One connection subscribed FTMS 2AD2 (349 frames), CP 2A63 (175) and FE-C FEC2 (174), while the V100 was connected at the same time (two peripherals on one adapter worked).

- **Cadence is reported under real pedalling.** FTMS instantaneous cadence 58–62 rpm. CP crank revolution/event deltas give a median of 59.5 rpm (range 57.9–61.9) over 86 intervals. FE-C page 25 cadence peaked at 62 rpm. The three sources agree.
- Power about 71–83 W across FTMS, CP and FE-C (FE-C maximum 82 W). Speed 18.4–19.8 km/h.
- FE-C FE state was 3 (in use) throughout, compared with 1 at idle.
- So the cadence of 0 during the hand-spin test came from the hand spinning (flywheel/cranks without rider load), not from missing support. *Inferred*; consistent with all captures.
- Sample frames are fixtures in the `bike-protocol` test `captured_x2_pedalling_frames`.

## Open items

- Accuracy against a trusted reference (power meter or head unit): no reference currently available.
- Whether the trainer accepts a second simultaneous central (for example MyWhoosh direct plus this reader): untested.
- FE-C frames while pedalling: check whether page 25 cadence/power and FE state change under load.
- FD00: purpose unknown; it emitted nothing while idle. No writes until there is evidence of the protocol.
