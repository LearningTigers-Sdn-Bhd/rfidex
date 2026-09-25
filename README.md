# RfiDex

RFID registration desk + entry/exit gate app for EventzFlow.

Design: `../docs/superpowers/specs/2026-09-25-rfidex-design.md` (§0, §2, §3).

Simulated devices only. There is no real-hardware adapter, no vendor DLL, no
installer and no rehearsal runner here: those are Plan 3 / P4.

## What is in the repository

- `rfidex-core` — device-neutral logic: tags, sticker codec, outbox, sync, stations.
- `rfidex-mock` — in-memory mock of the EventzFlow device API, for development and tests.
- `rfidex-runtime` — the application logic: per-station stores and workers, desk and
  gate views, problems, status, diagnostics. No Tauri types.
- `app/` — the desktop shell: React + Vite + TypeScript screens (`app/src`) and a thin
  Tauri 2 command layer (`app/src-tauri`). Every operator message is written in Rust;
  the UI renders it.

## Prerequisites

- Rust stable (the toolchain in `rust-toolchain` terms: `cargo 1.96`, `rustfmt` and
  `clippy` components).
- Node 20.19+ and npm (Vite 8 requires it). Only npm is used; there is no Bun setup.
- Tauri 2 system prerequisites for your OS (on macOS, Xcode command line tools; on
  Windows, WebView2 — present by default on Windows 10 21H2+ and 11).

## Develop

```bash
# everything headless
cargo test --workspace

# the mock EventzFlow server (development and tests only)
cargo run -p rfidex-mock -- \
  --port 4010 \
  --api-key <32+ characters, no spaces> \
  --tickets app/dev/tickets.json \
  --event-name "RfiDex Demo" \
  --mode bind
```

`app/dev/tickets.json` holds four fictional demo tickets (Aina, Ben, Chong, Devi).
`--mode write` starts the event in sticker-write mode instead of bind mode; the mode
is an event setting on the server, never a local switch.

```bash
# the desktop app
cd app
npm ci
npm run build          # tsc --noEmit && vite build
npm run tauri dev      # dev window against the Vite server on 127.0.0.1:1420
```

The app crate embeds `app/dist`, so **build the frontend before any cargo command that
builds `rfidex-app`** — that ordering is what CI does too.

## Setup, and what it deliberately does not have

There is **no PIN, no staff login and no per-device credential**. Setup opens from the
Setup button for anyone standing at the computer, by an explicit decision on
2026-09-25 to keep the system simple. Two things protect it instead:

- the saved API key is never sent back to the UI — Setup only learns whether a key
  exists, so editing shows `has_api_key` and a blank field;
- changing a gate's entry/exit direction asks for confirmation in a dialog that names
  the station and the move, because it changes what every later passage means.

Both the server address and the API key can be changed at any time, even with work
still queued: a revoked or wrong key is exactly what stops a queue draining, so
blocking the change would strand the work. Wrong-event delivery is prevented by the
event guard below, not by blocking setup.

Editing with a blank key keeps the saved one. A corrupt config file is reported as an
error rather than looking like a fresh install, so station UUIDs are never silently
lost. Removing a station while it still has pending or problem rows is refused, and a
removed station's database is **kept**, never deleted or reused for a different
station.

### The event guard

Sync pauses until a heartbeat has confirmed which event this station's rows belong to.
If the key has been moved to a different event while rows are waiting, the rows stay
queued, the saved settings are left alone, and the status line says so. With an empty
queue the station adopts the new event instead of blocking.

## Offline behaviour

Each station has its own SQLite store: ticket and binding cache, config, and a durable
outbox. Every write is committed (WAL, `synchronous = FULL`) before a function returns,
and a gate read is only released to the device after it is committed.

- Desk work continues from the cache while offline and says **"Offline — badge will
  print when connection returns."** Badge printing is triggered by the server after
  check-in, so an offline desk cannot print. This is a stated limitation, not a bug.
- A gate passage taken offline shows as **Recorded — waiting for the server**, never as
  accepted. When the server returns, the same local row becomes accepted or denied.
- **Sync now** means "try a normal sync pass now". It respects the retry backoff; it
  does not reset attempts or force a retry that is not due yet.

## Where the data lives

The app root is exactly Tauri's `app_local_data_dir()`:

| OS | Path |
|---|---|
| Windows | `%LOCALAPPDATA%\com.eventzflow.rfidex\` — per user, not ProgramData and not a shared installation folder |
| macOS | `~/Library/Application Support/com.eventzflow.rfidex/` |

```
config.json                 server address, API key, station list
stations/<uuid>.db          one database per station
exports/                    diagnostics CSV files
```

**Privacy and audit.** The API key is stored in plain text in `config.json`, and each
station database contains attendee names from the ticket cache. They are protected only
by the operating system's per-user permissions for that directory: there is no
encryption at rest and no isolation beyond the OS account. On a shared PC, any account
that can read those files can read the attendee names. This is recorded as an audit
item rather than a solved problem.

## Diagnostics

**Export diagnostics** writes an allowlisted CSV to `<app root>/exports/` and shows the
path. Columns:

```csv
station_id,row_id,kind,state,captured_at,attempts,uid_last4,outcome,problem_code
```

It never contains attendee names, ticket ids, full sticker UIDs, the API key, raw
server replies or memory payloads — those are not read while building the file, so they
cannot leak by accident. `uid_last4` is the last four characters of a validated UID, and
every cell is quoted with leading formula characters defused. A new export is always a
new file; a failed write removes the half-written one.

**Test connection** in Setup checks the address and key by reading one binding lookup.
It does not register a station, scan, bind or post anything.

## Stations, workers and what the screens show

One PC can run any mix of desk and gates. Each station owns its own store, its own API
client carrying its own UUID in `X-RfiDex-Station`, and its own sync worker.

- **Desk** — scan the ticket code (a keyboard-wedge scanner types into the focused
  field and presses Enter), then place one sticker. Two stickers are refused rather
  than guessed at. A sticker or ticket that is already linked asks for a reason before
  replacing it, and confirming one sticker never authorises a different one.
- **Gate** — the newest passage fills the top panel and older ones list below. A
  passage that has not reached the server says so in words, and server warnings are
  shown as sentences rather than enum names.
- **Problems** — conflicts and parked rows from every station, newest first.
  **"Dismiss from this list" hides the row; it does not delete it, retry it, or fix
  the server's answer.** The row keeps its payload and its server reply as evidence.
- **Simulator** — visible only for a simulated station, and visually separated from the
  operator screens. It stands in for hardware that is not here yet.

Status apart from the screen: a rejected API key is `unauthorized`, which is not the
same as offline; a failed disk measurement says so rather than reporting a healthy
value; and a quiet heartbeat cannot hide a local save or reader failure.

## Tests

```bash
cargo test --workspace        # 108 tests
cd app && npm run build       # type-check and bundle
cargo build -p rfidex-app     # the native binary
```

The runtime acceptance set runs the real `rfidex-runtime` against the real
`rfidex-mock` over HTTP — not against command mocks:

| Scenario | Test |
|---|---|
| Three stations register under their own UUIDs; the desk follows the event mode | `heartbeat_registers_three_stations`, `desk_follows_the_event_mode` |
| Link before scan, no tag, place tag, linked | `bind_flow_waits_for_scan_and_one_sticker` |
| Confirmation requires a reason and binds only the sticker warned about | `confirm_is_required_and_binds_only_the_sticker_that_was_warned_about` |
| A written sticker yields Welcome at entry and Goodbye at exit | `written_sticker_yields_welcome_and_goodbye` |
| Offline passage recorded, then accepted on the same row | `an_offline_passage_is_recorded_then_accepted_on_the_same_row` |
| Offline conflict names the holder and can be dismissed | `an_offline_conflict_names_the_holder_and_can_be_dismissed` |
| Export contains no names or full UID | `diagnostics_export_keeps_people_and_raw_uid_out` |
| A bad key is unauthorized, not offline | `a_rejected_key_is_unauthorized_rather_than_offline` |
| Connection check good and bad, with no side effects | `connection_test_reports_and_changes_nothing` |
| Shutdown finishes within its budget | `shutdown_finishes_even_when_the_server_stalls` |
| Event guard parks rows for another event | `event_guard_parks_rows_for_another_event` |

## Verification evidence

Screenshots and transcripts for the Plan 2 verification live outside this repository
in `../docs/rfidex-evidence/plan2/`.

- `native-run/` — the real Tauri window, in write mode, driven through the real screens
  by a temporary in-window fixture (deleted afterwards). The desk checked in two
  attendees, wrote and bound two stickers, and one sticker then passed both gates; the
  mock shows both bindings with `mode: "written"`, the gate databases show both
  passages accepted with no anomalies, and the block data each gate read decodes to the
  ticket that was written. A failure would have left an unknown sticker's denial in the
  entry database.
- `browser-*.png` — the operator screens rendered at 1366×768 with the Tauri IPC
  transport stubbed. **Supplementary only:** they show the layout, and the Rust side is
  never stubbed anywhere. Native screenshots could not be taken on the verification
  machine, which grants no Screen Recording permission.

Not verified on that machine: the Windows CI job (no remote may be pushed, and no
Windows SDK is available locally) and anything requiring a Windows build.

## Not in this plan

No installer is produced or certified, no rehearsal scenario runner exists, and no
real-hardware adapter is present or claimed to work. Those are Plan 3 and P4.
