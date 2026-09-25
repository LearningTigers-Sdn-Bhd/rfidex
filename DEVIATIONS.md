# Deviations from the plan

One entry per deviation: task, what changed, why.

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
