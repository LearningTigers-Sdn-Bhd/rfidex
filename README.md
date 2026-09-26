<div align="center">

<img src="app/app-icon.svg" alt="RfiDex" width="112" height="112" />

# RfiDex

**RFID registration desk and entry/exit gates for EventzFlow events.**<br/>
Scan a ticket, tag a sticker, walk through a gate — online or off.

[![CI](https://github.com/LearningTigers-Sdn-Bhd/rfidex/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/LearningTigers-Sdn-Bhd/rfidex/actions/workflows/ci.yml)
![Tests](https://img.shields.io/badge/tests-163_passing-2ea44f?style=for-the-badge&logo=checkmarx&logoColor=white)
![Platform](https://img.shields.io/badge/platform-Windows_x64-0078D4?style=for-the-badge&logo=windows&logoColor=white)
![Status](https://img.shields.io/badge/status-simulator_ready-f5a623?style=for-the-badge)

![Rust](https://img.shields.io/badge/Rust-stable-000000?style=flat-square&logo=rust&logoColor=white)
![Tauri](https://img.shields.io/badge/Tauri-2-24C8DB?style=flat-square&logo=tauri&logoColor=white)
![React](https://img.shields.io/badge/React-19-61DAFB?style=flat-square&logo=react&logoColor=black)
![TypeScript](https://img.shields.io/badge/TypeScript-7-3178C6?style=flat-square&logo=typescript&logoColor=white)
![Vite](https://img.shields.io/badge/Vite-8-646CFF?style=flat-square&logo=vite&logoColor=white)
![SQLite](https://img.shields.io/badge/SQLite-WAL-003B57?style=flat-square&logo=sqlite&logoColor=white)
![Tokio](https://img.shields.io/badge/Tokio-async-6E4A7E?style=flat-square)
![Axum](https://img.shields.io/badge/Axum-0.8_mock_server-7B3F00?style=flat-square)

[Features](#-features) ·
[Roadmap](#%EF%B8%8F-roadmap) ·
[Architecture](#%EF%B8%8F-architecture) ·
[Development](#-development) ·
[Operations](#-operations) ·
[Privacy](#-privacy)

</div>

---

## ✨ Features

<table>
<tr>
<td width="50%" valign="top">

### 🎫 Registration desk
- Keyboard-wedge QR scan — the scanner types, Enter submits
- **Search** by name, email or phone when the QR will not scan
- **Bind mode:** links the sticker's UID to the ticket
- **Write mode:** writes the ticket onto the sticker, reads it back, *and* binds the UID
- Sticker already in use? Shows whose it is and asks for a reason before replacing
- Two stickers on the reader are refused, never guessed
- Badge prints on a first check-in; **Reprint badge** whenever staff decide

</td>
<td width="50%" valign="top">

### 🚪 Entry & exit gates
- Large result panel: name, direction, **Welcome** / **Goodbye**
- The station's configured role is authoritative, whatever the reader reports
- Repeat entry, entry without check-in and payload mismatches shown as plain-language warnings
- Unknown sticker is denied — never a made-up name
- Records or live-inventory gates, with per-station debounce

</td>
</tr>
<tr>
<td valign="top">

### 📡 Offline-first
- Every action committed to SQLite **before** it's acknowledged
- Offline passages say **Recorded**, never *Accepted*
- Durable outbox with backoff; the same row flips to accepted on reconnect
- An offline desk never prints on its own — no false promises

</td>
<td valign="top">

### 🛡️ Safe by construction
- One PC runs any mix of desk + gates, each with its **own** store, client and UUID
- **Event guard:** queued work never reaches a different event
- Problems list: dismissing hides, never deletes evidence
- Diagnostics CSV is allowlisted — no names, no full UIDs, no key

</td>
</tr>
</table>

---

## 🗺️ Roadmap

```text
Overall  ███████░░░░░░░░░░░░░░  2 / 6 plans complete
```

| # | Phase | Scope | Status |
|:-:|:--|:--|:--|
| 1 | **P0 + P1** | Core logic, sticker codec, durable outbox, mock EventzFlow server | ![done](https://img.shields.io/badge/-done-2ea44f?style=flat-square) |
| 2 | **P2** | Tauri desktop app — setup, desk, gates, problems, simulator | ![done](https://img.shields.io/badge/-done-2ea44f?style=flat-square) |
| 3 | **P3** | 500-ticket simulated event rehearsal + Windows installer | ![next](https://img.shields.io/badge/-next-f5a623?style=flat-square) |
| 4 | **P4** | Real ECRFID desk and gate hardware | ![waiting](https://img.shields.io/badge/-on_hardware_arrival-lightgrey?style=flat-square) |
| 5 | **P5** | EventzFlow backend: RFID endpoints, visits, reports | ![approval](https://img.shields.io/badge/-needs_approval-lightgrey?style=flat-square) |
| 6 | **P6** | EventzFlow panel RFID tab | ![approval](https://img.shields.io/badge/-needs_approval-lightgrey?style=flat-square) |

```mermaid
flowchart LR
    P1["✅ P0+P1<br/>Core + mock"] --> P2["✅ P2<br/>Desktop app"]
    P2 --> P3["⏳ P3<br/>Rehearsal + installer"]
    P2 --> P4["⏳ P4<br/>Hardware"]
    P3 --> P5["🔒 P5<br/>Backend"]
    P4 --> P5
    P5 --> P6["🔒 P6<br/>Panel"]
    classDef done fill:#2ea44f,stroke:#1a7f37,color:#fff
    classDef next fill:#f5a623,stroke:#c78100,color:#000
    classDef locked fill:#d0d7de,stroke:#8c959f,color:#24292f
    class P1,P2 done
    class P3,P4 next
    class P5,P6 locked
```

---

## 🏗️ Architecture

```mermaid
flowchart TB
    subgraph APP["🖥️ app/ — Tauri 2 shell"]
        UI["React screens<br/>Setup · Desk · Gate · Problems · Simulator"]
        CMD["Thin command layer<br/>RwLock around the runtime"]
        UI -- "invoke()" --> CMD
    end

    subgraph RT["⚙️ rfidex-runtime"]
        RUN["Runtime<br/>per-station tasks · status · sync · shutdown"]
        VIEWS["Desk session · Gate views<br/>Problems · Diagnostics"]
    end

    subgraph CORE["🧩 rfidex-core"]
        ST["DeskStation / GateStation"]
        SYNC["SyncWorker + backoff"]
        STORE[("SQLite per station<br/>WAL · synchronous=FULL")]
        DEV["Device traits<br/>SimDesk · SimGate · ECRFID in P4"]
    end

    SERVER["☁️ EventzFlow API<br/>rfidex-mock in development"]

    CMD --> RUN --> VIEWS
    RUN --> ST
    RUN --> SYNC
    ST --> DEV
    ST --> STORE
    SYNC --> STORE
    SYNC -- "HTTPS · API key · X-RfiDex-Station" --> SERVER
```

| Crate | Role |
|:--|:--|
| [`rfidex-core`](crates/rfidex-core) | Device-neutral logic: tag keys, 20-byte sticker codec with CRC-8, outbox, sync, desk & gate stations, health |
| [`rfidex-mock`](crates/rfidex-mock) | In-memory EventzFlow device API with fault switches: down, delay, 5xx, hang-after-commit, bad body |
| [`rfidex-runtime`](crates/rfidex-runtime) | All application behaviour and every operator sentence — no Tauri types |
| [`app/`](app) | React + Vite + TypeScript screens and a thin Tauri command layer |

> [!TIP]
> Every decision and every message the operator reads is written in Rust. React only renders.

### 🔄 Desk flow

```mermaid
sequenceDiagram
    autonumber
    actor Staff
    participant Desk as RfiDex desk
    participant DB as Station SQLite
    participant API as EventzFlow

    Staff->>Desk: Scan ticket QR, or search name / email / phone
    Desk->>API: Desk scan (check-in)
    alt first check-in
        API-->>Desk: Checked in
        Desk->>Printer: Reprint badge (on this PC, no key)
    else already checked in
        API-->>Desk: Already checked in at 09:14 — Reprint offered
    else offline
        Desk->>DB: Queue scan, use cached ticket
        Desk-->>Staff: Offline — press Reprint when back online
    end
    Staff->>Desk: Place one sticker
    opt sticker already in use
        Desk-->>Staff: Belongs to someone else. Replace? (reason required)
    end
    Desk->>DB: Commit binding (write mode writes + reads back first)
    Desk->>API: Send binding now or on reconnect
    Desk-->>Staff: ✅ Sticker linked
```

---

## 🚀 Development

**Prerequisites:** Rust stable with `rustfmt` + `clippy` · Node 20.19+ with npm · [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/) (Xcode command line tools on macOS, WebView2 on Windows).

```bash
# 1 — the mock EventzFlow server (development only)
cargo run -p rfidex-mock -- \
  --port 4010 \
  --api-key rfidex_demo_key_0123456789abcdefghij \
  --tickets app/dev/tickets.json \
  --event-name "RfiDex Demo" \
  --mode bind            # or: write
```

```bash
# 2 — the desktop app
cd app
npm ci
npm run tauri dev        # native window, Vite on 127.0.0.1:1420
```

Then in **Setup**: server `http://127.0.0.1:4010`, the key above, then add one desk plus an entry and an exit gate. Demo tickets are fictional: Aina and Ben (valid), Chong (unpaid), Devi (cancelled). Each demo ticket carries a fictional email and phone so search can be tried.

```bash
# 3 — the badge printer app, on the same PC as the desk (optional)
#     event-printing, in direct mode: point it at a backend and an event slug.
#     RfiDex only asks it to reprint one ticket id; no key is sent to it.
python run_server.py            # listens on 127.0.0.1:8000 by default
```

> [!IMPORTANT]
> The app crate embeds `app/dist`. **Run `npm run build` before any cargo command that builds `rfidex-app`** — CI does the same.

---

## 🧪 Rehearsal and Windows packaging

The P3 acceptance run is a deterministic simulated event driven by the real runtime against the real mock over loopback HTTP — **500 tickets total, in two isolated event runs**: one **Bind** desk and one **Write** desk, each with an entry and an exit gate, run sequentially with separate mock state and separate data roots. Every run covers an entry burst, a logical lunch outage, a station restart, and a direction reported by the reader that contradicts the configured role.

- `rehearsal_smoke` — the same routine with 12 tickets (6 per event). The fast local check while developing the harness.
- `rehearsal_500` — the acceptance run: 250 tickets per event, 500 scan logs, 500 bindings and 2,000 observations, proven from the mock's own counters and delivery-ID sets.

```bash
cargo test --locked -p rfidex-runtime --test rehearsal rehearsal_smoke -- --ignored --exact --nocapture
cargo test --locked -p rfidex-runtime --test rehearsal rehearsal_500 -- --ignored --exact --nocapture
```

Both are `#[ignore]`d on purpose: the outage and restart stages wait on real retry backoff (1 s doubling to a 60 s cap), so they are far too slow for an ordinary push. They run **only locally and in the windows-package workflow** — ordinary CI stays as it is. Normal `cargo test --workspace` skips both and still runs the fast, explicit-clock fault and wrong-role probes.

Measured locally (macOS, debug build): smoke ≈ 0.9 s, full ≈ 2.4 s. The number is a duration, not a throughput claim.

**What the rehearsal proves:** every simulated registration and passage reaches the mock exactly once — its own counters and the exact delivery-ID set are the oracle, every result is accepted with no anomaly, each sticker has two entries and two exits, Write-mode stickers carry their own ticket and Bind-mode stickers carry nothing, and an outage, a restart and a recovery leave every queued row with the id and idempotency key it started with.

**What it does not prove:** RF range, UID byte order on real devices, device retention, vendor DLL behaviour, hardware timing, badge-printer delivery, or backend visit/headcount/duration rules. The lunch outage is a fault switch, not thirty minutes of wall clock, and the simulated sticker memory is volatile — a runtime restart loses it, so every passage is captured before the restart. Real hardware is P4.

> [!NOTE]
> **Status: the Windows installer is not built yet.** The NSIS packaging workflow and the clean-Windows manual acceptance are the rest of Plan 3. Nothing on this page is a claim that a Windows install was tested. The installer will be **unsigned** until a signing decision is made, and the embedded WebView2 bootstrapper **downloads the runtime when it is missing**, so a first install on a machine without WebView2 needs internet.

### 📦 Windows installer (unsigned)

The installer is built by a workflow of its own, never by ordinary CI:

```bash
gh workflow run windows-package.yml --repo LearningTigers-Sdn-Bhd/rfidex --ref main
gh run list --repo LearningTigers-Sdn-Bhd/rfidex --workflow windows-package.yml --limit 5
```

Against one checkout that run: fires the full rehearsal (smoke **and** 500); builds `rfidex.exe` and exactly one NSIS `*-setup.exe` for `x86_64-pc-windows-msvc` with `STATIC_VCRUNTIME=true`; builds `rfidex-mock.exe` with a static CRT as a **test tool** that is never part of the installer; audits every shipped binary's imports with `dumpbin /DEPENDENTS` and fails if a VC++ redistributable import appears; writes a SHA-256 manifest and the signing status. Nothing is uploaded until the rehearsal, the build and the import gate have all passed. A tag run also fails if the tag after `rfidex-v` disagrees with the declared version.

| Artifact | Contents |
|:--|:--|
| `rfidex-windows-x64-unsigned-<sha>-<run-id>` | the NSIS setup, `SHA256SUMS.txt`, rehearsal and import evidence |
| `rfidex-test-tools-<sha>-<run-id>` | `rfidex-mock.exe` and `tickets.json`, labelled test-only |

```bash
gh run download <run-id> --repo LearningTigers-Sdn-Bhd/rfidex --name rfidex-windows-x64-unsigned-<sha>-<run-id>
```

**Install prerequisites and behaviour**

- Windows 10 or 11, x64. The install is **per user**, so it does not need administrator rights.
- WebView2 is reused when it is already present. On a machine without it, the embedded bootstrapper **downloads** the runtime, so a first install needs internet — this is not an offline installer.
- No Visual C++ redistributable step: the release app links the static VC runtime, and the workflow fails the build if a shipped binary ever imports one again.
- The installer is **unsigned** until a signing decision is made, so Windows can show an Unknown publisher or SmartScreen warning. Never disable those protections to make an install pass.

### ✅ Checks

```bash
(cd app && npm ci && npm run build)
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # 163 tests
cargo build -p rfidex-app
```

<details>
<summary><b>📋 Runtime acceptance suite</b> — the real runtime against the real mock over HTTP</summary>
<br/>

| Scenario | Test |
|:--|:--|
| Three stations register under their own UUIDs | `heartbeat_registers_three_stations` |
| Desk follows the event's bind/write mode | `desk_follows_the_event_mode` |
| Link waits for a scan and exactly one sticker | `bind_flow_waits_for_scan_and_one_sticker` |
| A failed scan can't link the previous attendee | `a_failed_scan_cannot_link_the_previous_attendee` |
| Replacement needs a reason and binds only the warned sticker | `confirm_is_required_and_binds_only_the_sticker_that_was_warned_about` |
| Written sticker → Welcome at entry, Goodbye at exit | `written_sticker_yields_welcome_and_goodbye` |
| Offline passage recorded, then accepted on the same row | `an_offline_passage_is_recorded_then_accepted_on_the_same_row` |
| Offline conflict names the holder and can be dismissed | `an_offline_conflict_names_the_holder_and_can_be_dismissed` |
| Event guard parks rows for another event | `event_guard_parks_rows_for_another_event` |
| Rejected key is *unauthorized*, not *offline* | `a_rejected_key_is_unauthorized_rather_than_offline` |
| Connection test changes nothing on the server | `connection_test_reports_and_changes_nothing` |
| Diagnostics keep names and raw UIDs out | `diagnostics_export_keeps_people_and_raw_uid_out` |
| Shutdown finishes even when the server stalls | `shutdown_finishes_even_when_the_server_stalls` |

</details>

---

## 🧭 Operations

The operator screens use system fonts and bundled SVG artwork; no external fonts or CDNs are required. Station tabs separate registration from entry/exit gates, and status remains visible below the workspace.

Startup and browser-only screens pair a bundled window illustration with the original error details and recovery instructions. Desktop startup failures retain their error code and **Try again** action; the browser-only screen explains how to launch the desktop app instead of showing a misleading 404.

On Desk, the ticket-code field keeps scanner focus whenever no dialog is open. **Open simulator** opens hardware test controls in a dialog; closing it returns focus to the scanner. Replacement confirmation still requires a reason, and all runtime messages and decisions remain unchanged.

For a frontend focus smoke check, open a configured simulated Desk in the dev WebView and run `await (await import('/dev/desk-focus-check.js')).checkDeskFocus()` in its developer console. This checks focus recovery and modal isolation without scanning or linking a ticket.

### Finding a ticket

The QR scan is the main path; **Search by name, email or phone** is the fallback for a lost or unreadable code. One search box with a Name / Email / Phone switch:

| `by` | Rule | Minimum input |
|:--|:--|:--|
| name | anywhere in the name; case and spacing are ignored, and `%` / `_` are ordinary characters | 2 characters |
| email | the whole address, case-insensitive | contains `@` |
| phone | the digits only; a leading `60` or `0` is dropped, so `012-345 6789`, `0123456789` and `+60 12 345 6789` find the same guest | 4 digits |

Paid tickets only, newest first, **at most 10 rows**, and email and phone are **masked by the server** — only a hint such as `ah***@example.com` or `•••• 4521` reaches this computer. A row says `Checked in 09:14` when the guest is already in; that time is this PC's own clock, 24-hour, so **set the desk PC's timezone correctly**. Selecting a row runs the ordinary desk scan with that ticket, so the sticker step is exactly the same.

Offline, RfiDex searches **its own saved ticket list by name only** — the local cache holds no email or phone, by design, so an email or phone search offline says `Needs internet — search by name or scan QR` instead of pretending to have searched. An offline result list is ordered by name, not newest first, and a checked-in guest shows `Checked in` with no time, because the cache has none.

### Badge printing

RfiDex prints through **event-printing**, the badge printer app on the desk's own PC. Each desk has its own **Printer address** in Setup, default `http://127.0.0.1:8000`, with a **Test printer** button that reports the printer app's name. Only a loopback address is accepted: a printer on another PC is not supported yet. RfiDex asks event-printing to reprint **one ticket id** and event-printing applies its own badge layout, so the badge mapping lives in one place and **no API key is sent to the printer app**.

event-printing must be set up in **direct mode on that PC**: the EventzFlow backend URL and the event slug, so it can look the ticket up itself.

| When | What happens |
|:--|:--|
| The scan made the **first** check-in | One badge is printed once. The guest stays on screen until the answer arrives. |
| The guest was **already** checked in | No automatic print. The screen says `Already checked in at HH:MM` and offers **Reprint badge**. |
| The scan was **offline** (queued) | No print at all. The screen says `Offline — press Reprint when back online`. |
| Reprint, or after a failure | **Reprint badge**, as many times as staff decide. |

Printing is fire-and-report: it never blocks the sticker step, and a bad sticker, a failed print or a slow printer never stops a guest being linked. The timeout is 10 seconds, and a timeout is reported as a failure because the job may already have printed — RfiDex never retries by itself. Nothing prints from a queue drain, a cache refresh, a heartbeat or a restart: only staff asking.

> [!WARNING]
> **The SalesCatalyst print workflow must stay OFF for RfiDex events — keep it built as a backup only.** The `ticket.scanned` webhook fires for RfiDex check-ins too, and its payload does not say which app checked the guest in, so a workflow that prints on that webhook produces **two badges per guest**. Switch it on only if RfiDex printing fails at the event and staff stop using it. On each desk PC event-printing runs in direct mode instead.

### Setup — simple on purpose

There is **no PIN, no staff login and no per-device credential.** Instead:

- the saved API key is **never sent back** to the screen — editing shows a blank field, and leaving it blank keeps the saved key;
- changing a gate between **entry and exit** asks for confirmation, because it changes what every later passage means;
- the server address and key can change at any time, even with work queued — the **event guard** holds that work back from a different event. With an empty queue, a new event is simply adopted.

### Status at a glance

| You see | It means |
|:--|:--|
| 🟢 **Accepted** — Welcome / Goodbye | The server confirmed the passage |
| 🟡 **Recorded — waiting for the server** | Saved locally; sends on reconnect |
| 🔴 **Denied** + reason | Unknown, replaced, wrong-event or invalid sticker |
| 🟠 **Problem** | Conflict or unsendable row — see Problems |
| **Unauthorized** | The server rejected the API key (not the same as offline) |
| **Different event** | The key belongs to another event while work is queued — rows held |

> [!NOTE]
> **Sync now** means "try a normal pass now" — it respects retry backoff.
> **Dismiss from this list** hides a problem; it does not delete, retry or fix it.

### Where data lives

| OS | Path |
|:--|:--|
| 🪟 Windows | `%LOCALAPPDATA%\com.eventzflow.rfidex\` (per user) |
| 🍎 macOS | `~/Library/Application Support/com.eventzflow.rfidex/` |

```text
config.json          server address, API key, station list
stations/<uuid>.db   one SQLite database per station (kept even if the station is removed)
exports/             diagnostics CSV files
```

---

## 🔒 Privacy

> [!WARNING]
> The API key is stored in plain text in `config.json`, and station databases hold attendee names from the ticket cache. They are protected **only by the OS user account's file permissions** — there is no encryption at rest. Use a dedicated Windows account on shared PCs.

The **diagnostics export** is built from an allowlist, so personal data can't leak by accident:

```csv
station_id,row_id,kind,state,captured_at,attempts,uid_last4,outcome,problem_code
```

No names, ticket IDs, full UIDs, API key, server replies or sticker memory. Every cell is quoted and leading formula characters (`= + - @`) are defused.

---

<div align="center">
<sub>Built for EventzFlow · Windows x64 · Simulator-verified, hardware pending (P4)</sub>
</div>
