// Demo bridge for styling previews. Loaded only when the dev server sees
// ?preview=1 (see api.ts); it poses as the Tauri internals so the real
// screens render with believable data, no Rust side needed. Never shipped:
// preview mode is dev-only, and Vite removes this module from production builds.

import type { BadgeSettings, BadgeSettingsView, AppView, DeskView, GateView, SearchView, SetupView, StationStatus, UpdateView, VerifyView } from "./api";

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
  status: { print_provider: "Badge printing: event-printing app", stations: [desk, gate], pending: 3, problems: 1, alarm: null },
};

let badgeSettings: BadgeSettings = {
  native_print_enabled: false, printer: "Zebra ZD421", thermal: false, rotate_90: false, badge_types: ["Sponsor"], presets: {}, active_preset: null,
  layout: {paper:{width_mm:100,height_mm:80},elements:["name","role","company","qr"],custom_fields:{},element_scales:{},element_bolds:{},element_offsets:{},vertical_offset_mm:0},
};
const provider = () => badgeSettings.native_print_enabled ? "Badge printing: built-in" : "Badge printing: event-printing app";
const badgeView = (): BadgeSettingsView => ({ settings: structuredClone(badgeSettings), printers: {names:["Zebra ZD421","Card printer"],default:"Zebra ZD421",supported:true}, warning:null,provider:provider() });

function answer(cmd: string, args: Record<string, unknown> = {}): unknown {
  switch (cmd) {
    case "badge_get": return badgeView();
    case "badge_save": {
      const enabled = badgeSettings.native_print_enabled;
      badgeSettings = structuredClone(args.settings as BadgeSettings);
      badgeSettings.native_print_enabled = enabled;
      return badgeView();
    }
    case "badge_set_enabled": {
      badgeSettings.native_print_enabled = args.enabled === true;
      if (appView.status) appView.status.print_provider = provider();
      return {native_print_enabled:badgeSettings.native_print_enabled,provider:provider()};
    }
    case "badge_ticket_types":
      return ["Delegate", "Speaker", "VIP", "Visitor"];
    case "badge_test_print":
    case "badge_print":
      return "Preview only. No badge was printed.";
    case "badge_import": {
      badgeSettings.layout.paper = {width_mm:104,height_mm:155};
      badgeSettings.presets.Card = structuredClone(badgeSettings.layout);
      badgeSettings.active_preset = "Card";
      return badgeView();
    }
    case "badge_preview": {
      const ticket = args.ticket as {name:string;company:string};
      const escape = (value:string) => value.replaceAll("&","&amp;").replaceAll("<","&lt;").replaceAll(">","&gt;");
      const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="500" height="400"><rect width="500" height="400" fill="white"/><text x="250" y="100" text-anchor="middle" font-family="Arial,sans-serif" font-size="30">${escape(ticket.name)}</text><text x="250" y="150" text-anchor="middle" font-family="Arial,sans-serif" font-size="20">${escape(ticket.company)}</text><text x="250" y="290" text-anchor="middle" font-family="sans-serif" font-size="16">UI preview only</text></svg>`;
      return `data:image/svg+xml,${encodeURIComponent(svg)}`;
    }
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
      return { current: "0.7.0", available: null, notes: null } satisfies UpdateView;
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
    case "export_problems":
      return "/tmp/rfidex-problems.csv";
    default:
      throw new Error(`preview: no demo answer for ${cmd}`);
  }
}

declare global {
  interface Window {
    __TAURI_INTERNALS__?: { invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown> };
  }
}

export function installPreviewBridge() {
  window.__TAURI_INTERNALS__ = {
    invoke: (cmd: string, args?: Record<string, unknown>) => Promise.resolve().then(() => answer(cmd,args)),
  };
}
