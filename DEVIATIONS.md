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
