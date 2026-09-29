// The invoke boundary: one helper per Rust command, with the exact DTO shapes
// the runtime serializes. Nothing here decides what a failure means — the Rust
// `RuntimeError.message` is what the operator reads.
import { invoke } from "@tauri-apps/api/core";

export type RfidMode = "bind" | "write";
export type StationKind = "desk" | "gate";
export type Role = "entry" | "exit";
export type GateKind = "records" | "live_inventory";
export type SearchBy = "name" | "email" | "phone";

export type EnumerationKind = "hid" | "com" | "net";

/** How a real reader is reached. Tags match the Rust `HardwareConfig`. */
export type HardwareConfig =
  | {
      transport: "ec_v19_plain_tcp";
      address: string;
      bus_address: number;
      antenna_byte: boolean;
      timeout_ms: number;
    }
  | {
      transport: "ecrfid_sdk";
      dll_path: string;
      connection: SdkConnection;
      inventory_mode: number;
      timeout_ms: number;
      write_verified?: boolean;
    };

export type SdkConnection =
  | {
      kind: "hid";
      model: string;
      path: string;
      address_mode: number;
      exclusive: number;
    }
  | {
      kind: "com";
      model: string;
      port: string;
      baud: number;
      frame: string;
      bus_address: number;
    }
  | {
      kind: "net";
      model: string;
      interface: string;
      address: string;
    };

export type DeviceChoice =
  | { type: "sim_desk" }
  | { type: "sim_gate"; gate_kind: GateKind; release_verified: boolean }
  | { type: "ecrfid_desk"; hardware: HardwareConfig }
  | { type: "ecrfid_gate"; hardware: HardwareConfig };

/** A real reader station, whichever way it is reached. */
export type RealDevice =
  | { type: "ecrfid_desk"; hardware: HardwareConfig }
  | { type: "ecrfid_gate"; hardware: HardwareConfig };

export const isRealStation = (device: DeviceChoice): device is RealDevice =>
  device.type === "ecrfid_desk" || device.type === "ecrfid_gate";

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
  connection_checked: boolean;
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

/** The two reader tests an operator may run. Neither writes to a sticker. */
export type HardwareTestAction = "connect" | "read_tags" | "gate_records";

export interface HardwareTestView {
  ok: boolean;
  message: string;
  uid_raw_hex: string[];
  /** Always false in this build: only physical acceptance can change it. */
  hardware_verified: boolean;
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
export const hardwareEnumerate = (dllPath: string, kind: EnumerationKind) =>
  invoke<string[]>("hardware_enumerate", { dllPath, kind });
export const hardwareTest = (station: string, action: HardwareTestAction) =>
  invoke<HardwareTestView>("hardware_test", { station, action });
export type StickerTestStep = "status" | "check" | "tear_write" | "tear_check";

export interface StickerTally {
  passed: number;
  failed: number;
  tears: number;
  tears_undetected: number;
  target_stickers: number;
  target_tears: number;
}

export interface StickerTestView {
  ok: boolean;
  message: string;
  details: string[];
  tally: StickerTally;
  tear_waiting: boolean;
  write_enabled: boolean;
  can_enable: boolean;
}

export const hardwareDiscover = (dllPath: string, iface: string) =>
  invoke<string[]>("hardware_discover", { dllPath, iface });
export const stickerTest = (station: string, step: StickerTestStep) =>
  invoke<StickerTestView>("sticker_test", { station, step });
export const stickerWriting = (station: string, on: boolean) =>
  invoke<AppView>("sticker_writing", { station, on });
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

/** Styling preview: dev builds may add ?preview=1 to render pages in a plain
 *  browser; every backend call then fails harmlessly. */
const previewMode = import.meta.env.DEV && new URLSearchParams(window.location.search).has("preview");

/** True only inside the Tauri window; a plain browser has no bridge to Rust. */
export const inDesktopApp = () => "__TAURI_INTERNALS__" in window || previewMode;

/**
 * Messages written by Rust are shown as they are. Anything else (a JavaScript
 * exception, a Tauri transport error) is logged for developers and shown as a
 * plain operator message; raw internal text never reaches the screen.
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
  if (previewMode) {
    return { code: "preview", message: "Preview only: this page is not connected to the RfiDex app." };
  }
  return {
    code: "app_error",
    message:
      "Something unexpected went wrong inside RfiDex. Try again. If it keeps happening, restart RfiDex and tell support what you pressed.",
  };
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
