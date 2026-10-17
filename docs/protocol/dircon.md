# DirCon (Wahoo Direct Connect) — protocol notes

DirCon carries BLE GATT operations over TCP. OsmolControl uses it to present a virtual trainer to MyWhoosh on the LAN.

**Sources:** behavioural description from the open-source qdomyos-zwift DirCon implementation (GPL-3.0; `src/devices/dircon/dirconpacket.{h,cpp}`, `dirconprocessor.cpp`, `dirconmanager.cpp`; master branch, fetched 2026-10-17). It was used as a protocol reference only; no code was copied. Wahoo publishes no specification. Everything here is *documented by a third-party implementation* until confirmed live against MyWhoosh.

## Discovery

- mDNS service `_wahoo-fitness-tnp._tcp.local.`; instance name = the display name; port = the TCP server port.
- TXT: `mac-address` (colon hex), `serial-number`, `ble-service-uuids` (comma-separated full 128-bit UUIDs, e.g. `00001826-0000-1000-8000-00805f9b34fb`).
- Hostname = the display name with spaces replaced by `-`, IPv4 only. The reference notes that MyWhoosh matches on it, and that an AAAA record without a listener causes problems.

## Framing

Each message has a 6-byte header followed by a payload:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 1 | version = 1 |
| 1 | 1 | message id |
| 2 | 1 | sequence number (a response echoes the request's; notifications use 0) |
| 3 | 1 | response code |
| 4 | 2 | payload length, big-endian |

TCP reads are not message boundaries: accumulate and parse by length.

Message ids: 0x01 discover services, 0x02 discover characteristics, 0x03 read, 0x04 write, 0x05 enable notifications, 0x06 unsolicited notification, 0x07 unknown (answer success with no payload), 0xFF error.

Response codes: 0x00 success/request, 0x01 unknown message type, 0x02 unexpected error, 0x03 service not found, 0x04 characteristic not found, 0x05 operation not supported, 0x06 write failed, 0x07 unknown protocol.

## Payloads (UUIDs are 16 bytes, big-endian text order)

| Message | Request | Response |
| --- | --- | --- |
| 0x01 discover services | empty | N × service UUID |
| 0x02 discover characteristics | service UUID | service UUID + N × (char UUID + 1 property byte) |
| 0x03 read | char UUID | char UUID + value |
| 0x04 write | char UUID + data | char UUID (+ optional data) |
| 0x05 enable notifications | char UUID + 1 byte (1 on, 0 off) | char UUID |
| 0x06 notification | — | char UUID + value (server → client, seq 0) |

Property bits: 0x01 read, 0x02 write, 0x04 notify, 0x08 indicate.

## Behaviour notes

- Send **nothing** on connect. The reference reports that an early frame made MyWhoosh disable 2AD2 (Indoor Bike Data) and fall back to CSC.
- Send notifications only for characteristics the client enabled.
- No keepalive or idle timeout is defined.
- FTMS Control Point (2AD9) writes are answered with a DirCon write response. The FTMS result comes later, as a 2AD9 notification (indication over BLE).

## Live results — MyWhoosh 5.7.0 on macOS (2026-10-17)

- **mDNS:** the 18-byte service name `_wahoo-fitness-tnp` exceeds RFC 6763's 15-byte limit. mdns-sd rejected it silently until `set_service_name_len_max(18)` was applied; daemon errors are now logged.
- MyWhoosh discovered the virtual trainer through **LAN** and showed **"OsmolControl"** on the trainer tile with live power and cadence from the X2 Max (user-confirmed).
- Control traffic observed: one Request Control (0x00), then *Set Indoor Bike Simulation* (0x11) at about 1 Hz, grade −1.0 % to +10.9 % (80 commands in the session sample). Nothing was refused by the safety filter. No ERG commands yet. Wind 0, Crr 0x35, Cw 0x33 as sent by MyWhoosh.
- MyWhoosh's own virtual shifting stayed active (shifts were displayed in MyWhoosh). OsmolControl gear templates are **not** applied yet.
