# Library gate alarm: handoff (2026-09-29)

Read this first if you are the Claude session on the Windows PC with the gate attached.

## Where things stand

- v0.6.10 (`2d8fb5c`) added the alarm: a declined pass calls `LibraryGateAlarm(0x01)`
  through `EcrfidGate::alarm` (`crates/rfidex-hardware/src/adapters.rs`), sent from
  `alarm_declined` in `crates/rfidex-runtime/src/runtime.rs`. Declined means the
  sticker is unknown, unbound, revoked, invalid or not checked in
  (`gate::admitted` in `crates/rfidex-core/src/station/gate.rs`).
- After v0.6.10 the operator saw: gate stuck red with the buzzer, correct bound
  stickers not recorded, RfiDex showing "Waiting" (no pass captured). Deleting and
  recreating the gate station did not help. A second gate read the same sticker fine.
- v0.6.11 (`32209f6`, tag `rfidex-v0.6.11`) is a **blind, untested fix**: `alarm()`
  now sets `started = false` so the next poll restarts the fetch with 0x02.

## Hypotheses (none confirmed)

1. `LibraryGateAlarm` sent between a fetch (0x02/0x01) and the next ack breaks the
   gate's record handshake: the ack is ignored, the same pass is served again, and
   later passes queue behind it. v0.6.11 targets this.
2. Mode 0x01 left the gate latched in an alarm state until it is power-cycled.
   "Waiting" means RfiDex captured nothing, and it only alarms after storing a pass,
   so the red light and buzzer came from the gate itself. Power-cycling the first
   gate has not been tried yet.
3. The vendor demos never send the alarm during polling (it is a manual button in
   `library-gate/Form1.cs`), so interleaving is the odd part of our design.

## Open questions

- Green flash on an accepted pass. The vendor video shows a green flash with no
  sound for accepted passes, red plus sound for declined. The library gate SDK has
  no green command. `MeetingGateAlarm(light, mode, count, HT, LT)` does
  (light `0x00` green / `0x01` red; mode `0x00` sound / `0x01` light / `0x02` both;
  count; HT; LT), but it is documented for the meeting gate. Untested on this hardware.
- The resting green is the gate's own breathing light (Config16 byte 4, set in the
  vendor D-tool). It is not something RfiDex drives.

## To do on the Windows PC (in order)

1. Power-cycle the gate that got stuck. Walk the correct sticker. Note: green and
   recorded, or red again?
2. With the vendor demo (`docs/rfid-vendor/reference/extracted/`, in the eventzflow
   workspace, outside this repo): press the `library-gate` demo's alarm button, then
   check that passes still read. That shows whether the alarm freezes the fetch.
3. In the `meeting-gate` demo, send green, light only (light 0x00, mode 0x01,
   count 1, HT 1, LT 1). Does the library gate flash green?
4. Install v0.6.11 and check: unknown sticker gives red plus buzzer once; a bound,
   checked-in sticker right after gets recorded; gate returns to green at rest.

## Decisions

- If step 3 works, add a background green flash for accepted passes as v0.6.12,
  ignoring errors and never blocking the pass. Do not send it inline in the poll
  loop: interleaved commands are the suspected cause of the stuck fetch.
- If the alarm still breaks reads, gate it behind a setting that is off by default.
  That restores v0.6.9 behaviour (green at rest, passes recorded).
- Do not change the alarm mode or timing without a device result; the docs are
  ambiguous.
