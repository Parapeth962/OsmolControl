# Bluetooth read contract

Bluetooth SIG standard characteristic layouts: Fitness Machine / Indoor Bike Data (2AD2), Fitness Machine Feature (2ACC), supported resistance (2AD6) and power (2AD8) ranges; Cycling Power Measurement (2A63) and Feature (2A65); CSC Measurement (2A5B) and Feature (2A5C); Battery Level (2A19), Device Information (180A). UUIDs use the standard Bluetooth base. Standard formats are not evidence that every device supports them.

Indoor Bike Data begins with LE16 flags. Instantaneous speed is present when bit 0 is clear, in 0.01 km/h. Bits 1–12 add average speed, instantaneous/average cadence (0.5 rpm), 24-bit distance (m), resistance (signed 0.1), instantaneous/average power (signed W), energy (u16 kcal, u16 kcal/h, u8 kcal/min), heart rate (u8 bpm), metabolic equivalent (0.1), elapsed/remaining time (u16 s), in that order. Missing fields remain absent. Reject reserved flags, truncated fields and unexplained trailing bytes.

CPS: LE16 flags and signed LE16 watts, followed by optional fields in SIG order; crank revolution data uses u16 total and u16 event ticks at 1024 Hz. CSC uses flags followed by optional wheel (u32, u16 at 1024 Hz) and crank (u16, u16 at 1024 Hz). Cadence requires two fresh samples with wrapping counters; no revolution change must not preserve stale cadence indefinitely. Raw revolution counters are reported without inventing instantaneous cadence.

Vendor characteristics remain raw unless an explicit capture-backed mapping is configured. V100 has no validated default mapping. Read-only operation never requests FTMS control or changes resistance. Notifications subscribe only to supported standard measurements/status or an explicitly configured custom characteristic.
