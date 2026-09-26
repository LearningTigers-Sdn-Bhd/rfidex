// The invoke boundary: one helper per Rust command, with the exact DTO shapes
// the runtime serializes. Nothing here decides what a failure means — the Rust
// `RuntimeError.message` is what the operator reads.
import { invoke } from "@tauri-apps/api/core";

export type RfidMode = "bind" | "write";
export type StationKind = "desk" | "gate";
export type Role = "entry" | "exit";
export type GateKind = "records" | "live_inventory";
export type SearchBy = "name" | "email" | "phone";

export type DeviceChoice =
  | { type: "sim_desk" }
  | { type: "sim_gate"; gate_kind: GateKind; release_verified: boolean };

export interface StationConfig {
  id: string;
  name: string;
  kind: StationKind;
  role: Role | null;
  device: DeviceChoice;
  debounce_secs: number;
  write_start_block: number;
  /** The badge printer app on this PC. Only a desk ever prints. */
  printer_url: string;
}

export interface SetupView {
  server_url: string;
  has_api_key: boolean;
  stations: StationConfig[];
}

export interface SetupInput {
  server_url: string;
  api_key: string;
  stations: StationConfig[];
}

export interface ConnectionView {
  ok: boolean;
  code: string;
  message: string;
}

export interface TicketSummary {
  public_id: string;
  name: string;
  ticket_type: string;
  valid: boolean;
  checked_in: boolean;
}

export type DeskStep = "ready" | "scanned" | "linked" | "needs_confirm" | "error";

/**
 * What Rust decided about this guest's badge. `print_now` is the one signal to
 * call `deskPrint`; every sentence here is Rust's, including the check-in time.
 */
export interface BadgeView {
  print_now: boolean;
  can_reprint: boolean;
  /** Keep the guest on screen (no 3 s auto-reset) until staff act. */
  hold: boolean;
  message: string | null;
}

export interface DeskView {
  step: DeskStep;
  code: string | null;
  message: string;
  ticket: TicketSummary | null;
  offline: boolean;
  mode: RfidMode;
  /** Identifies the guest on screen; a print result may only land on its own. */
  session_id: string;
  badge: BadgeView;
}

export interface SearchRow {
  public_id: string;
  name: string;
  ticket_type: string;
  /** Masked by the server; never present offline. */
  email_hint: string | null;
  phone_hint: string | null;
  /** "Checked in 09:14" online, "Checked in" offline, null if not in. */
  checked_in_message: string | null;
}

export interface SearchView {
  offline: boolean;
  /** The minimum-input hint, "No matching tickets", or the offline message. */
  message: string | null;
  rows: SearchRow[];
}

export type GateStatus = "recorded" | "accepted" | "denied" | "problem";

export interface GateView {
  id: number;
  captured_at: string;
  status: GateStatus;
  role: Role;
  name: string | null;
  message: string;
  anomalies: string[];
}

export interface ProblemView {
  station_id: string;
  station_name: string;
  id: number;
  captured_at: string;
  name: string | null;
  message: string;
}

export interface StationStatus {
  id: string;
  name: string;
  kind: StationKind;
  role: Role | null;
  simulated: boolean;
  online: boolean;
  unauthorized: boolean;
  connected: boolean;
  event_name: string | null;
  mode: RfidMode | null;
  settings_ready: boolean;
  skew_secs: number | null;
  last_sync: string | null;
  last_error: string | null;
  pending: number;
  problems: number;
  alarm: string | null;
}

export interface AppStatus {
  stations: StationStatus[];
  pending: number;
  problems: number;
  alarm: string | null;
}

export interface AppView {
  configured: boolean;
  status: AppStatus | null;
}

export interface UpdateView {
  current: string;
  available: string | null;
  notes: string | null;
}

export const appState = () => invoke<AppView>("app_state");
export const setupGet = () => invoke<SetupView | null>("setup_get");
export const setupSave = (input: SetupInput) => invoke<AppView>("setup_save", { input });
export const setupTest = (url: string, key: string) =>
  invoke<ConnectionView>("setup_test", { url, key });
export const setupTestPrinter = (url: string) =>
  invoke<ConnectionView>("setup_test_printer", { url });
export const status = () => invoke<AppStatus>("status");
export const deskScan = (station: string, code: string) =>
  invoke<DeskView>("desk_scan", { station, code });
export const deskLink = (station: string, reason: string | null) =>
  invoke<DeskView>("desk_link", { station, reason });
export const deskReset = (station: string) => invoke<DeskView>("desk_reset", { station });
export const deskSearch = (station: string, by: SearchBy, query: string) =>
  invoke<SearchView>("desk_search", { station, by, query });
export const deskPrint = (station: string, sessionId: string) =>
  invoke<BadgeView>("desk_print", { station, sessionId });
export const gateRecent = (station: string, limit = 30) =>
  invoke<GateView[]>("gate_recent", { station, limit });
export const problems = () => invoke<ProblemView[]>("problems");
export const dismissProblem = (station: string, id: number) =>
  invoke<boolean>("dismiss_problem", { station, id });
export const syncNow = () => invoke<void>("sync_now");
export const exportDiagnostics = () => invoke<string>("export_diagnostics");
export const simPlace = (station: string, uidHex: string) =>
  invoke<void>("sim_place", { station, uidHex });
export const simClear = (station: string) => invoke<void>("sim_clear", { station });
export const simPass = (station: string, uidHex: string) =>
  invoke<void>("sim_pass", { station, uidHex });
export const simSetConnected = (station: string, connected: boolean) =>
  invoke<void>("sim_set_connected", { station, connected });
export const updateCheck = () => invoke<UpdateView>("update_check");
export const updateInstall = () => invoke<void>("update_install");

/**
 * A failed command carries the Rust message. Anything else is a transport
 * problem, not a second place that decides what an RFID error means.
 */
/** A failure the Rust side wrote for the operator: `RuntimeError` serialized. */
export interface AppFailure {
  code: string;
  message: string;
}

const UNKNOWN_FAILURE: AppFailure = {
  code: "app_error",
  message: "The app could not finish this action.",
};

/** True only inside the Tauri window; a plain browser has no bridge to Rust. */
export const inDesktopApp = () => "__TAURI_INTERNALS__" in window;

/**
 * Only messages from Rust reach the operator. A JavaScript error (an `Error`
 * instance) is logged for developers and shown as the neutral fallback, so a
 * raw "Cannot read properties of undefined" never appears on screen.
 */
export function failureOf(failure: unknown): AppFailure {
  if (
    typeof failure === "object" &&
    failure !== null &&
    !(failure instanceof Error) &&
    typeof (failure as AppFailure).code === "string" &&
    typeof (failure as AppFailure).message === "string"
  ) {
    return failure as AppFailure;
  }
  console.error(failure);
  return UNKNOWN_FAILURE;
}

export const errorText = (failure: unknown): string => failureOf(failure).message;

export function formatTime(value: string | null): string {
  if (!value) return "—";
  const at = new Date(value);
  if (Number.isNaN(at.getTime())) return "—";
  return at.toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}
