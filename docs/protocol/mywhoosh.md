# OpenBikeControl network contract

Source: official MIT specification commit 20c063d9452a057e5f723e54987a987d4d6d41a9, MDNS.md and PROTOCOL.md. Verified against upstream application strings/source, not a live MyWhoosh session.

Rust is the TCP server; MyWhoosh discovers _openbikecontrol._tcp.local. and connects. TXT fields: version=1, unique id, name, service-uuids=d273f680-d548-419d-b9d1-fa0472345229, manufacturer, model. Port is selected/configured locally and the actual bound port is advertised. Prefer explicit LAN IPv4 to avoid advertising VPN/container addresses.

Device-to-app button transition: 01 <button> <state>; shift up=01, down=02; state 01 press, 00 release. Status: 02 <battery or ff> <ready>. Send status every 30 seconds. App-to-device haptic: 03 <pattern> <duration> <intensity>, four bytes; unsupported haptics are reported but not written to a device. App info: 04 01 <id length> <UTF8 id> <version length> <UTF8 version> <button count> <buttons>. Each string limited to 32 bytes here; count zero means all buttons. App info is optional; clear capabilities on disconnect.

Inbound TCP parsing must accumulate fragmented messages and consume coalesced messages by field-derived lengths. Bound storage and close malformed sessions. No invented acknowledgement or keepalive message. The specification lacks a general length envelope for outgoing variable-length button batches; send only single-button transitions and validate interoperability with the real client. TCP writes are NOT message boundaries; do not claim otherwise. Status and haptic frames are fixed-length.

Current implementation scope: controller actions only, no trainer telemetry forwarding. Live MyWhoosh validation and multi-platform runtime testing remain outstanding.

## Live observations — MyWhoosh 5.7.0 on macOS (2026-10-17)

Setup: `dist/bikectl connect-mywhoosh --bind 192.168.1.10 --controller <V100> --mapping v100.local.json` on Linux; MyWhoosh on a Mac at 192.168.1.20, same LAN. 

- **Discovery works:** MyWhoosh found the mDNS record and connected without manual IP entry. The Linux host needed a ufw rule allowing TCP 36867 from the Mac (UDP 5353 was already allowed).
- **App info:** MyWhoosh sends `04 01 …` with id `mywhoosh`, version `5.7.0`.
- **Undocumented keepalive (observed):** about 3 s after connecting, and then every 4 s, MyWhoosh sends a single byte `00`. Type 0x00 is not in the specification. The original parser rejected it and closed the session, so MyWhoosh reconnected in a loop (9 connect/drop cycles at 4 s intervals in `mywhoosh-live-1.log`). Fix: unknown app-to-device types are now dropped instead of being fatal; a lone `00` is logged at trace level as a keepalive. Because the protocol has no length envelope, an unknown type discards the rest of the current read buffer.
- **Shifting works end to end:** V100 FEA1 → `bikectl` → OpenBikeControl press/release → MyWhoosh virtual shifting. The tester confirmed that MyWhoosh shifts on + and −. 51 actions were sent in one session; presses as close as 220 ms apart were forwarded. The tester described the shifting as "not fluid, sometimes sticks or lags, but manageable". The server does not measure MyWhoosh's reaction time, so where the delay comes from (V100 delivery, the server, MyWhoosh, or the trainer's resistance ramp) is **unresolved**. Dropped stale actions (older than 500 ms or while not ready) are now logged at debug level to help separate these.
- The trainer was paired to MyWhoosh directly over the Mac's Bluetooth (power, cadence, controllable); this app was not connected to the trainer at the time.
- After the keepalive fix and a swapped V100 mapping (`mywhoosh-live-3.log`), the tester reported shifting is faster and more reliable, with the correct direction. This is a subjective report and latency is still unmeasured; the earlier sluggishness is consistent with the 4 s reconnect loop, which dropped presses that fell between sessions (*inferred*).
