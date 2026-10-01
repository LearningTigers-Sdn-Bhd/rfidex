// Demo bridge for styling previews. Loaded only when the dev server sees
// ?preview=1 (see api.ts); it poses as the Tauri internals so the real
// screens render with believable data, no Rust side needed. Never shipped:
// preview mode is dev-only, and Vite removes this module from production builds.

import type { AppView, DeskView, GateView, SearchView, SetupView, StationStatus, UpdateView, VerifyView } from "./api";

const minutesAgo = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();

const desk: StationStatus = {
  id: "desk-1",
  name: "Front desk",
  kind: "desk",
  role: null,
  simulated: true,
  online: true,
  unauthorized: false,
  connected: true,
  connection_checked: true,
  event_name: "Summit 2026",
  mode: "bind",
  settings_ready: true,
  skew_secs: 0,
  last_sync: minutesAgo(1),
  last_error: null,
  pending: 0,
  problems: 0,
  alarm: null,
};

const gate: StationStatus = {
  id: "gate-1",
  name: "Hall A entrance",
  kind: "gate",
  role: "entry",
  simulated: true,
  online: true,
  unauthorized: false,
  connected: true,
  connection_checked: true,
  event_name: "Summit 2026",
  mode: "bind",
  settings_ready: true,
  skew_secs: 1,
  last_sync: minutesAgo(2),
  last_error: null,
  pending: 3,
  problems: 1,
  alarm: null,
};

const appView: AppView = {
  configured: true,
  status: { stations: [desk, gate], pending: 3, problems: 1, alarm: null },
};

function answer(cmd: string): unknown {
  switch (cmd) {
    case "app_state":
      return appView;
    case "status":
      return appView.status;
    case "setup_get":
      return {
        server_url: "https://events.example.com", has_api_key: true,
        stations: [
          { id: "desk-1", name: "Front desk", kind: "desk", role: null, device: { type: "sim_desk" }, debounce_secs: 5, write_start_block: 0, printer_url: "http://127.0.0.1:8000", alarm_all_panels: true, alarm_wait_ms: 1500 },
          { id: "gate-1", name: "Hall A entrance", kind: "gate", role: "entry", device: { type: "sim_gate", gate_kind: "records", release_verified: false }, debounce_secs: 5, write_start_block: 0, printer_url: "", alarm_all_panels: true, alarm_wait_ms: 1500 },
        ],
      } satisfies SetupView;
    case "update_check":
      return { current: "0.6.23", available: null, notes: null } satisfies UpdateView;
    case "problems":
      return [
        {
          station_id: "gate-1",
          station_name: "Hall A entrance",
          id: 12,
          captured_at: minutesAgo(5),
          name: "Ada Lovelace",
          message: "The server sent a reply this app cannot read. Ask for help.",
        },
      ];
    case "gate_recent": {
      const samples = [
        { id: 1, status: "accepted", name: "Ada Lovelace", role: "entry", message: "Welcome", captured_at: minutesAgo(1), anomalies: [] },
        { id: 2, status: "recorded", name: "Alan Turing", role: "exit", message: "Recorded — waiting for the server.", captured_at: minutesAgo(3), anomalies: [] },
        { id: 3, status: "denied", name: null, role: "entry", message: "This sticker has been replaced.", captured_at: minutesAgo(7), anomalies: [] },
      ] satisfies GateView[];
      if (new URLSearchParams(window.location.search).get("gate-demo") !== "long-names") return samples;
      const names = [
        "Alexandria Catherine Elizabeth Montgomery-Wellington",
        "Muhammad Alexander Iskandar bin Abdullah Rahman",
        "AlexandriaCatherineElizabethMontgomeryWellingtonWithoutSpaces",
      ];
      return Array.from({ length: 30 }, (_, index) => ({
        ...samples[index % samples.length],
        id: index + 1,
        name: index % 3 === 2 ? null : names[Math.floor(index / 3) % names.length],
        captured_at: minutesAgo(index + 1),
      }));
    }
    case "desk_search":
      return { offline: false, message: null, rows: [{ public_id: "DEMO-0012", name: "Ada Lovelace", ticket_type: "VIP", email_hint: "a***@example.com", phone_hint: null, checked_in_message: null }] } satisfies SearchView;
    case "desk_scan":
    case "desk_link":
      return {
        step: "linked", code: null, message: "Sticker linked. Ready for the next guest.",
        ticket: { public_id: "DEMO-0012", name: "Ada Lovelace", ticket_type: "VIP", valid: true, checked_in: true },
        offline: false, mode: "bind", session_id: "preview-session",
        badge: { print_now: false, can_reprint: false, hold: true, message: "Preview only. No badge was printed." },
      } satisfies DeskView;
    case "desk_reset":
      return {
        step: "ready", code: null, message: "Scan a ticket to begin.", ticket: null,
        offline: false, mode: "bind", session_id: "preview-reset",
        badge: { print_now: false, can_reprint: false, hold: false, message: null },
      } satisfies DeskView;
    case "desk_verify":
      return { state: "waiting", code: null, message: "Hold your tag near the reader", holder: null, sticker: null } satisfies VerifyView;
    case "sync_now":
      return null;
    case "export_diagnostics":
      return "/tmp/rfidex-diagnostics.csv";
    default:
      throw new Error(`preview: no demo answer for ${cmd}`);
  }
}

declare global {
  interface Window {
    __TAURI_INTERNALS__?: { invoke: (cmd: string) => Promise<unknown> };
  }
}

export function installPreviewBridge() {
  window.__TAURI_INTERNALS__ = {
    invoke: (cmd: string) => Promise.resolve(answer(cmd)),
  };
}
