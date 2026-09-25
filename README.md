# RfiDex

RFID registration desk + entry/exit gate app for EventzFlow.
Design: `../docs/superpowers/specs/2026-09-25-rfidex-design.md`.

## Crates

- `rfidex-core` — device-neutral logic (tags, sticker codec, outbox, sync, stations).
- `rfidex-mock` — in-memory mock of the EventzFlow device API, for development and tests.

## Develop

```bash
cargo test --workspace
cargo run -p rfidex-mock -- --port 4010 --api-key <32+ chars> --tickets tickets.json
```
