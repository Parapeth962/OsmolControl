# Fixture provenance

Current fixtures are synthetic, hand-encoded from the documented wire layouts, not captured from the ThinkRider devices. See bike-protocol unit tests and bike-mywhoosh TCP tests.

Indoor Bike Data: `4400c409b400ecff` = speed 25 km/h, cadence 90 rpm, power -20 W. Flags 0x0044. CPS: `2000fa00ffff0004` = 250 W, crank count 65535, event tick 1024. OBC shift up press/release: `010101` / `010100`. OBC app info (all actions supported): `0401000000`.

Add real fixtures only after captures with device model, firmware, action labels and timestamp, removing addresses/serials where irrelevant. Never label synthetic data as V100 validation.

## Captured fixtures (2026-10-17)

Source: local captures (not published). Linux, BlueZ 5.87. Device names/addresses omitted.

- **X2 Max**, ThinkRider fw V3.2.3 / hw V1.1.0, steady pedalling at about 60 rpm. FTMS 2AD2 `44009b077c005200` = 19.47 km/h, 62 rpm, 82 W. CP 2A63 `30005300ec01000053763900a4e9` = 83 W, wheel 492 revs @ 30291/2048 s, crank 57 revs @ 59812/1024 s. Used in the `bike-protocol` test `captured_x2_pedalling_frames`.
- **V100** (advertises `Think VS01-…`), fw V1.0.5, characteristic FEA1. The tester pressed + five times, then − five times, repeatedly (8 + and 7 − groups of 5). Each press produced exactly one frame: `f3050300fb` = shift up (+), `f3050301fc` = shift down (−). No release or repeat frames. Used in the `bike-core` test `captured_v100_clicks`.
