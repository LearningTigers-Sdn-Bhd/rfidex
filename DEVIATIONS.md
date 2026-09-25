# Deviations from the plan

One entry per deviation: task, what changed, why.

## Plan 2 / post-run audit fixes (2026-09-25)

Found by the Claude audit after Task 9.

1. **Event guard failed open** (`runtime.rs`, `heartbeat_once`). A failed queue count was read as zero, so a heartbeat for a different event would be adopted and the queued rows sent to it. The count now fails closed (`u64::MAX`), keeping the saved event. No test: a store read failure after a successful open cannot be produced without a fault hook, and adding one only for this is not worth it.
2. **Desk result could land on another desk** (`App.tsx`). Station screens were not keyed, so a request in flight for one desk could set the view of the next desk selected. The station wrapper is now keyed by UUID, which remounts Desk, Gate and Simulator on a switch; the manual reset effects that tried to do this were removed.
3. **Cleanup, no behaviour change** (`runtime.rs`): dropped an unused captured value, an unused error parameter, an `unreachable!` arm, and a duplicated unauthorized message.
4. **Commit messages reworded** with `git filter-branch --msg-filter` (no remote existed; backup branch `backup/pre-reword-plan2`): `feat(runtime): show gate results and problems` gained both co-author trailers it lacked, two subjects were shortened to at most 50 characters, and two bodies rewrapped at 72. Trees are byte-identical to the backup.

## Plan 2 / Task 9 — the walkthrough was driven by a temporary fixture, and Windows CI still could not run

- **What changed:** the operator walkthrough was driven inside the real window by a temporary in-window fixture (`app/src/devdrive.ts`, imported from `main.tsx`), which typed and clicked the real screens: two desk runs in write mode and one passage each way through the gates. The fixture was deleted and `main.tsx` restored after the run; nothing of it is committed. Evidence is in `../docs/rfidex-evidence/plan2/native-run/`. The `windows-app` job result remains unverified.
- **Why:** the machine grants neither Screen Recording (`screencapture`: `could not create image from display`; `CGWindowListCopyWindowInfo` returns the window's geometry but with its title redacted) nor assistive access (`osascript ... is not allowed assistive access (-1719)`), so the window can be driven only from inside itself. The fixture leaves its evidence outside the window — station databases, diagnostics exports, and the mock — and marks a failure by walking an unknown sticker past the entry gate, which a clean run never does. The plan's Task 9 Step 5 already allows a temporary developer fixture for exactly this. GitHub Actions cannot be run without pushing or creating a remote, which this run is not allowed to do, and a Windows cross-build is not possible from macOS without the Windows SDK.

## Plan 2 / Task 8 — `src/vite-env.d.ts` for the CSS import

- **What changed:** added `app/src/vite-env.d.ts` containing `/// <reference types="vite/client" />`. The file map does not list it.
- **Why:** `npm run build` failed with `src/main.tsx(5,8): error TS2882: Cannot find module or type declarations for side-effect import of './style.css'` under TypeScript 7.0.2. The reference is the standard Vite declaration file and is the documented way to type CSS side-effect imports; nothing else about the build changed.

## Plan 2 / Task 8 — native screenshots are not available on this machine

- **What changed:** the operator screens were captured as browser renders of the real React app against a stubbed Tauri IPC transport, written as a throwaway Playwright script in the session scratchpad. No harness file was added to the repository.
- **Why:** `screencapture` fails here with `could not create image from display`, and AppleScript UI scripting is refused with `osascript is not allowed assistive access (-1719)`, so neither a native grab nor native interaction is possible. The plan's Task 8 Step 5 allows a browser-only render as supplementary evidence. The Rust side is not stubbed anywhere: it is covered by the runtime acceptance tests against the real mock, and the live window was verified to start the stations and persist their heartbeats, which only happens if the real IPC path ran.

## Plan 2 / Task 4 — a heartbeat no longer discards a pending confirmation

- **What changed:** `DeskSession` records the UID rule already applied to it, and `DeskSession::apply_settings` clears the pending confirmation only when the heartbeat's mode or UID rule actually differs. Before, every successful heartbeat cleared it. Committed on its own as `fix(runtime): keep confirmations across heartbeats`, after the Task 7 gate exposed it.
- **Why:** the plan says "Clear pending confirmation if heartbeat changes event mode or UID rule", but the first version cleared it unconditionally, so a heartbeat landing between the warning and the operator's answer threw the answer away. `confirm_is_required_and_binds_only_the_sticker_that_was_warned_about` failed about one run in three with `NeedsConfirm` where it expected `Linked`; with the fix it passed 10 consecutive runs of the whole test file. The test predates the fix, so it is the regression test.

## Plan 2 / Task 4 — the first sync pass waits for the heartbeat, not a poll interval

- **What changed:** `StationRuntime` gained a `tokio::sync::Notify`. The heartbeat calls `notify_one()` when it records that syncing is allowed, and the sync loop waits on `sleep(1s)`, that notification, and the stop signal.
- **Why:** Task 3's event guard parks the sync loop until the first successful heartbeat, and the loop's 1-second wait runs before that. So the first cache refresh happened up to a second after the station was already online — long enough that `an_offline_desk_says_the_badge_prints_later` saw an empty cache and a server-down scan reported `ticket_not_found` instead of showing the cached ticket. Waking on the heartbeat makes "immediate pass" true the moment the guard lifts, instead of a poll interval later. No interval, backoff or retention value changed.

## Plan 2 / Task 3 — `desk.rs` created one task early

- **What changed:** `crates/rfidex-runtime/src/desk.rs` exists after Task 3 instead of first appearing in Task 4. It holds only `DeskView`, `DeskStep`, `PendingConfirm` and the `DeskSession` constructor; Task 4 adds the transitions.
- **Why:** Task 3 Step 3 owns `StationDevice::Desk(tokio::sync::Mutex<DeskSession>)`, so the session type has to compile before Task 4's file map says the file is created. No behaviour is implemented early.

## Plan 2 / Task 3 — clippy `large_enum_variant` on `StationDevice`

- **What changed:** both variants are boxed — `Desk(Box<tokio::sync::Mutex<DeskSession>>)` and `Gate(Box<tokio::sync::Mutex<GateStation<GateDevice>>>)` — with a comment. The enum, its matching, and every call site are otherwise unchanged.
- **Why:** clippy 1.96 reports `large size difference between variants` (`-D warnings`), first at 448 vs 232 bytes and again at 232 vs 8 once only the desk was boxed. Boxing both is one allocation per station at startup and removes the lint without an `#[allow]`.

# Deviations from `docs/superpowers/plans/2026-09-25-rfidex-1-core-and-mock.md`

One entry per deviation: task, what changed, why.

## Task 1 — commit co-author trailer

- **What changed:** commits end with both `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` (as the plan's Global Constraints require) and `Co-authored-by: DeepSeek V4.1 Flash <noreply@commandcode.ai>`, which the plan does not mention.
- **Why:** user decision on 2026-09-25 during Task 1 review — keep the plan's trailer and add one naming the model that produced the work, in the formal name form used by the Claude trailer. No plan step behaved unexpectedly; no test failed.

## Task 2 — `cargo fmt` reformats the plan's pasted code

- **What changed:** every task's pasted source is rustfmt-clean only after running `cargo fmt --all`; it wraps several long `assert_eq!` lines and `let` bindings. Formatting only — no logic, no test assertion changed.
- **Why:** `cargo fmt --all -- --check` is a Global Constraint and an expected-clean gate in Task 1 Step 5, but the plan's snippets exceed rustfmt's default 100-column width. Running `cargo fmt` is the plan's own remedy; adding a `rustfmt.toml` to widen the limit would have been an unrequested config change.

## Task 2 — clippy `manual_is_multiple_of` on `parse_hex`

- **What changed:** `if s.len() % 2 != 0` became `if !s.len().is_multiple_of(2)` in `tag.rs`.
- **Why:** clippy 1.96 (the plan's toolchain) rejects the modulo form under `-D warnings`; the plan's Step 5 expects clippy clean. Same behaviour, and the odd-length test still covers it.

## Task 6 — same clippy lint in `SimDesk::write_blocks`

- **What changed:** `if data.len() % t.block_size != 0` became `if !data.len().is_multiple_of(t.block_size)`.
- **Why:** same `manual_is_multiple_of` lint as Task 2. The block-alignment test in `range_alignment_and_support_errors` still asserts `OutOfRange` for unaligned writes.

## Task 8 — clippy `result_large_err` on `ApiFailure`

- **What changed:** `check_ticket`, `desk_scan` and `bind` in `rfidex-mock/src/state.rs` carry `#[allow(clippy::result_large_err)]` with a comment; the `ApiFailure = (u16, ErrorBody)` alias, every signature and every test are unchanged.
- **Why:** clippy 1.96 warns that the `Err` variant of `(u16, ErrorBody)` is ~192 bytes, and the plan's gate is warnings-as-errors. Boxing it would have changed the alias that Task 8's interface section fixes and that Tasks 9–12 consume, so the scoped allow keeps the documented contract. Nothing about behaviour or test coverage changed.

## Task 10 — reqwest `query` needs an explicit feature

- **What changed:** the workspace `reqwest` entry became `features = ["json", "query"]` (was `["json"]`).
- **Why:** in reqwest 0.13.5 `RequestBuilder::query` sits behind the `query` feature (`dep:serde_urlencoded`), so `ApiClient::lookup` failed to compile with `no method named query found`. The plan's Task 10 note explicitly prescribes enabling the named feature rather than hand-rolling the query string. No call site or test changed.

## Task 12 — clippy `result_large_err` on `DeskStation`

- **What changed:** `station/desk.rs` opens with a documented module-level `#![allow(clippy::result_large_err)]`; `DeskError`, its variants and all five signatures are exactly as the plan writes them.
- **Why:** `DeskError::Api(ApiError)` carries `ApiError::Rejected`'s `ErrorBody` (~192 bytes), so clippy 1.96 rejects the `Result<_, DeskError>` returns under warnings-as-errors. Boxing would change the error enum that Task 12's interface lists and that the tests pattern-match on (`Err(DeskError::NeedsConfirm(..))`), so the allow is scoped to the one module whose fallible methods all return it.

## Post-plan audit fixes (2026-09-25)

Found in the plan's own design during the audit after Task 14; each has a regression test that fails on the old code.

1. **Gate release could lose reads** (`station/gate.rs`). One failed `release()` aborted the batch, so later reads in the same poll were never saved (on a fetch-consumes gate: lost). `tick` now saves every read first, then releases all, reporting the first failure. Test: `gate::release_failure_does_not_lose_later_reads` (old code saved 1 of 3). `SimGate` gained `fail_next_releases`.
2. **Undecodable outbox row stopped all sync** (`sync.rs`). A row whose payload no longer decodes (e.g. after an app upgrade) made every `run_once` fail. It is now parked and the queue continues. Test: `sync::undecodable_rows_are_parked_not_blocking`.
3. **Unreadable 2xx reply retried forever** (`client.rs`, `sync.rs`). New `ApiError::BadResponse`; retried with backoff up to `MAX_BAD_RESPONSE_ATTEMPTS` (5), then parked. Network errors still retry forever. Test: `sync::unreadable_server_reply_is_parked_after_retries`; mock `Faults` gained `bad_body`.
4. **Write mode could write before learning the ticket already has a sticker** (`station/desk.rs`). An online scan now caches the ticket's current sticker, so `link` warns before writing. Test: `desk::write_mode_checks_existing_sticker_before_writing` (old code wrote the sticker).
5. **Sent rows were never deleted** (`store.rs`, `sync.rs`). `Store::prune_sent(older_than)`; `run_forever` prunes sent rows older than `SENT_RETENTION_DAYS` (7) hourly. Pending, conflict and parked rows are never pruned. Test: `store::prune_sent_removes_only_old_sent_rows`.
