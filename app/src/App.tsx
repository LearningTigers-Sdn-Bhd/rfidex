import { useCallback, useEffect, useState } from "react";
import type { ReactNode } from "react";

import { appState, errorText, failureOf, inDesktopApp, status, syncNow } from "./api";
import type { AppFailure, AppStatus, AppView, StationStatus } from "./api";
import { Desk } from "./Desk";
import { Gate } from "./Gate";
import { Help } from "./Help";
import { Problems } from "./Problems";
import { Setup } from "./Setup";
import { Simulator } from "./Simulator";
import { Verify } from "./Verify";

/** The status bar refreshes after each answer, never on an overlapping clock. */
const STATUS_MS = 1000;

type Tab = "station" | "verify" | "problems" | "help" | "setup";

type Theme = "light" | "dark" | "system";
const THEME_ORDER: Theme[] = ["system", "light", "dark"];
const THEME_LABEL: Record<Theme, string> = {
  system: "System theme",
  light: "Light theme",
  dark: "Dark theme",
};

function initialTheme(): Theme {
  const attr = document.documentElement.dataset.theme;
  return attr === "light" || attr === "dark" ? attr : "system";
}

/** Round sun / moon / monitor toggle cycling system → light → dark. */
function ThemeToggle({ theme, onCycle }: { theme: Theme; onCycle: () => void }) {
  return (
    <button
      type="button"
      className="theme-toggle"
      onClick={onCycle}
      title={`Theme: ${THEME_LABEL[theme]} — click to change`}
      aria-label={`${THEME_LABEL[theme]}. Activate to switch theme.`}
    >
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <g className="icon-sun">
          <circle cx="12" cy="12" r="4.2" />
          <path d="M12 2.5v2.2M12 19.3v2.2M4.3 4.3l1.6 1.6M18.1 18.1l1.6 1.6M2.5 12h2.2M19.3 12h2.2M4.3 19.7l1.6-1.6M18.1 5.9l1.6-1.6" />
        </g>
        <g className="icon-moon">
          <path d="M20.4 14.2A8.5 8.5 0 0 1 9.8 3.6a8.5 8.5 0 1 0 10.6 10.6Z" />
        </g>
        <g className="icon-auto">
          <rect x="3" y="4.5" width="18" height="12" rx="2" />
          <path d="M9.5 20.5h5" />
        </g>
      </svg>
      <span className="nav-label">Appearance</span>
    </button>
  );
}

/** Inline icon set: one stroke style, 1.7px, drawn on a 24 grid. */
function Icon({ d, paths }: { d?: string; paths?: readonly string[] }) {
  return (
    <svg className="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      {d ? <path d={d} /> : paths?.map((path) => <path key={path} d={path} />)}
    </svg>
  );
}

const ICONS = {
  stations: [
    "M3 4h18v13H3Z",
    "M8 21h8M12 17v4",
    "M6 8h5M6 12h3M15 8h3M13 12h5",
  ],
  verify: [
    "M3 8V4h4M17 4h4v4M21 16v4h-4M7 20H3v-4",
    "M7 9h5v6H7Z",
    "m15 12 2 2 4-4",
  ],
  problems: "M12 8.2v5M12 16.8h.01M10.2 3.9 2.8 17a2 2 0 0 0 1.7 3h15a2 2 0 0 0 1.7-3L13.8 3.9a2 2 0 0 0-3.6 0Z",
  help: "M9.2 9a2.9 2.9 0 0 1 5.6 1c0 1.9-2.6 2.4-2.6 3.9M12 17.2h.01M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18Z",
  gear: "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6ZM19 12a7 7 0 0 0-.14-1.4l2-1.55-2-3.46-2.36.95a7 7 0 0 0-2.42-1.4L13.7 2.6h-3.4l-.38 2.54a7 7 0 0 0-2.42 1.4l-2.36-.95-2 3.46 2 1.55A7 7 0 0 0 5 12c0 .48.05.94.14 1.4l-2 1.55 2 3.46 2.36-.95a7 7 0 0 0 2.42 1.4l.38 2.54h3.4l.38-2.54a7 7 0 0 0 2.42-1.4l2.36.95 2-3.46-2-1.55c.09-.46.14-.92.14-1.4Z",
  sync: "M20 11a8 8 0 0 0-14.9-3M4 13a8 8 0 0 0 14.9 3M4 4v4h4M20 20v-4h-4",

} as const;

export function App() {
  const [view, setView] = useState<AppView | null>(null);
  const [fatal, setFatal] = useState<AppFailure | null>(null);
  const [tab, setTab] = useState<Tab>("station");
  const [selected, setSelected] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [theme, setTheme] = useState<Theme>(initialTheme);

  // Own the <html> attribute so the toggle, the bootstrap script and
  // localStorage always agree.
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    try {
      localStorage.setItem("rfidex-theme", theme);
    } catch {
      // Private windows can refuse storage; the attribute still applies.
    }
  }, [theme]);

  const cycleTheme = () =>
    setTheme((current) => THEME_ORDER[(THEME_ORDER.indexOf(current) + 1) % THEME_ORDER.length]);

  const load = useCallback(async () => {
    try {
      const next = await appState();
      setView(next);
      setFatal(null);
      return next;
    } catch (problem) {
      setFatal(failureOf(problem));
      return null;
    }
  }, []);

  const desktop = inDesktopApp();

  useEffect(() => {
    if (desktop) void load();
  }, [desktop, load]);

  const configured = view?.configured ?? false;

  useEffect(() => {
    if (!configured) return;
    let live = true;
    let timer: number | undefined;
    const tick = async () => {
      try {
        const next = await status();
        if (!live) return;
        setView((current) => (current ? { ...current, status: next } : current));
      } catch {
        // A failed poll says nothing new; the station rows carry the reason.
      }
      if (live) timer = window.setTimeout(tick, STATUS_MS);
    };
    timer = window.setTimeout(tick, STATUS_MS);
    return () => {
      live = false;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [configured]);

  if (!desktop) {
    return (
      <StartupProblem
        title="Open RfiDex from its app window"
        detail="This page is the development server, not the app. RfiDex talks to its stations through the desktop window, which a web browser cannot provide."
        help={<>Start the app with <code>npm run tauri dev</code> in <code>rfidex/app</code>, or open the installed RfiDex.</>}
      />
    );
  }

  if (fatal) {
    return (
      <StartupProblem
        title={STARTUP_TITLES[fatal.code] ?? "RfiDex could not start"}
        detail={fatal.message}
        code={fatal.code}
        onRetry={() => {
          setFatal(null);
          void load();
        }}
      />
    );
  }

  if (view === null) {
    return (
      <div className="app">
        <main className="workspace">
          <p>Starting…</p>
        </main>
      </div>
    );
  }

  if (!view.configured) {
    return (
      <div className="app">
        <main className="workspace">
          <Setup
            hasSavedConfig={view.configured}
            status={view.status}
            onSaved={(saved) => {
              setView(saved);
              setNotice("Setup saved.");
            }}
            onCancel={() => setTab("station")}
          />
        </main>
      </div>
    );
  }

  const stations = view.status?.stations ?? [];
  // A station that disappeared after a save must not leave the screen blank.
  const station: StationStatus | null =
    stations.find((candidate) => candidate.id === selected) ?? stations[0] ?? null;

  // Verify reads the first desk reader; only a desk can be asked.
  const verifyDesk = stations.find((candidate) => candidate.kind === "desk") ?? null;
  const onVerify = tab === "verify" && verifyDesk !== null;

  const runSync = async () => {
    setBusy(true);
    setNotice(null);
    try {
      await syncNow();
      await load();
      setNotice("Sync finished. Anything the server refused is in Problems.");
    } catch (problem) {
      setNotice(errorText(problem));
    } finally {
      setBusy(false);
    }
  };

  const problemsCount = view.status?.problems ?? 0;

  const navItems: { id: Tab; label: string; icon: string | readonly string[]; badge?: number }[] = [
    { id: "station", label: "Console", icon: ICONS.stations },
    ...(verifyDesk ? [{ id: "verify" as Tab, label: "Verify", icon: ICONS.verify as string | readonly string[] }] : []),
    { id: "problems", label: "Problems", icon: ICONS.problems, badge: problemsCount },
    { id: "help", label: "Help", icon: ICONS.help },
    { id: "setup", label: "Setup", icon: ICONS.gear },
  ];

  return (
    <div className="app">
      <aside className="rail">
        <div className="rail-mark" aria-hidden="true" title="RfiDex">
          r.
        </div>
        <nav className="rail-nav" aria-label="Sections">
          {navItems.map((item) => (
            <button
              key={item.id}
              type="button"
              className={tab === item.id ? "rail-btn is-active" : "rail-btn"}
              aria-current={tab === item.id ? "page" : undefined}
              title={item.label}
              onClick={() => setTab(item.id)}
            >
              <Icon {...(typeof item.icon === "string" ? { d: item.icon } : { paths: item.icon })} />
              <span>{item.label}</span>
              {item.badge != null && item.badge > 0 && <em className="rail-badge">{item.badge}</em>}
            </button>
          ))}
        </nav>
        <div className="rail-spacer" />
        <ThemeToggle theme={theme} onCycle={cycleTheme} />
      </aside>

      <div className="shell">
        <Ticker status={view.status} />

        <header className="topbar">
          <span className="topbar-title">RfiDex</span>
          <span className="topbar-crumb">Event operations</span>
          <div className="topbar-actions">
            <button type="button" className="topbar-btn" aria-label={busy ? "Sync in progress" : "Sync"} title="Sync queued records" onClick={() => void runSync()} disabled={busy}>
              <Icon d={ICONS.sync} />
              <span>{busy ? "Working…" : "Sync"}</span>
            </button>
          </div>
        </header>

        <main className={onVerify ? "workspace is-stage" : "workspace"}>
        {tab === "help" ? (
          <Help />
        ) : tab === "setup" ? (
          <Setup
            hasSavedConfig={view.configured}
            status={view.status}
            onSaved={(saved) => {
              setView(saved);
              setTab("station");
              setNotice("Setup saved.");
            }}
            onCancel={() => setTab("station")}
          />
        ) : onVerify ? (
          // Keyed: a desk switch never keeps an old answer up.
          <Verify key={verifyDesk.id} station={verifyDesk} />
        ) : stations.length === 0 ? (
          <section className="empty-state">
            <h2>No stations on this computer yet</h2>
            <p>Open Setup and add the desk and gates this PC runs.</p>
            <button type="button" onClick={() => setTab("setup")}>
              Open Setup
            </button>
          </section>
        ) : tab === "problems" ? (
          <Problems problemCount={view.status?.problems ?? 0} />
        ) : (
          station && (
            <div className="console">
              <nav className="station-nav" aria-label="Stations">
                {stations.map((candidate) => {
                  const connected = candidate.connection_checked && candidate.connected;
                  const kindLabel = candidate.kind === "desk" ? "Desk" : candidate.role === "exit" ? "Exit" : "Entry";
                  return (
                    <button
                      key={candidate.id}
                      type="button"
                      className={candidate.id === station.id ? "station-tab is-active" : "station-tab"}
                      aria-current={candidate.id === station.id ? "page" : undefined}
                      onClick={() => setSelected(candidate.id)}
                    >
                      <span className={`station-tab-led${connected ? " on" : ""}`} aria-hidden="true" />
                      <span className="station-tab-name">{candidate.name}</span>
                      <span className="station-tab-kind">{kindLabel}</span>
                    </button>
                  );
                })}
              </nav>

              <header className="console-head">
                <div className="console-id">
                  <div className="console-meta">
                    <span className="console-kind">{station.kind === "desk" ? "Registration desk" : station.role === "exit" ? "Exit gate" : "Entry gate"}</span>
                    {station.event_name && <span className="console-event">{station.event_name}</span>}
                    <span className={`connection${!station.connection_checked ? " unknown" : station.connected ? "" : " disconnected"}`}>
                      {!station.connection_checked ? "Reader not checked" : station.connected ? "Reader connected" : "Reader disconnected"}
                    </span>
                    {station.simulated && <span className="simulation-label">Simulated</span>}
                  </div>
                </div>
              </header>

              {/* Keyed: a station switch remounts its screens, so a request
                  still in flight for the old station can never land on the new one. */}
              <div className={`console-body is-${station.kind}`} key={station.id}>
                <div className="console-work">
                  {station.kind === "desk" ? (
                    <Desk station={station} />
                  ) : (
                    <Gate station={station} />
                  )}
                </div>
                <aside className="live-rail" aria-label="Session activity">
                  <h2 className="rail-head">Session</h2>
                  <dl className="rail-stats">
                    <div><dt>Queued</dt><dd>{view.status?.pending ?? 0}</dd></div>
                    <div><dt>Attention</dt><dd className={problemsCount > 0 ? "is-warn" : ""}>{problemsCount}</dd></div>
                    <div><dt>Stations</dt><dd>{stations.length}</dd></div>
                  </dl>
                  <div className="rail-block">
                    <h3>About this station</h3>
                    {station.kind === "desk" ? (
                      <ol className="rail-steps">
                        <li><b>Scan the ticket</b><span>The guest’s name appears beside the scanner.</span></li>
                        <li><b>Tap the sticker</b><span>One sticker on the reader. Wait for “Linked”.</span></li>
                        <li><b>Hand it over</b><span>Check the badge status, then hand over tag and badge.</span></li>
                      </ol>
                    ) : (
                      <ol className="rail-steps">
                        <li><b>Guests tap as they pass</b><span>Every tap lands on the stage within half a second.</span></li>
                        <li><b>Watch the feed</b><span>Anything unusual is flagged on its row.</span></li>
                      </ol>
                    )}
                  </div>
                </aside>
              </div>
              {import.meta.env.DEV && <Simulator station={station} />}
            </div>
          )
        )}

        {notice && <p className="note">{notice}</p>}
      </main>
      </div>
    </div>
  );
}

/** The razor-thin system readout pinned above everything. */
function Ticker({ status }: { status: AppStatus | null }) {
  const offline = status?.stations.filter((station) => !station.online) ?? [];
  const readers = status?.stations.filter((station) => station.connection_checked && !station.connected) ?? [];
  const errors = status?.stations.filter((station) => station.last_error) ?? [];
  const online = status !== null && offline.length === 0;
  const headline = status === null
    ? "Connecting"
    : online
      ? "Online"
      : `Offline at ${offline.length} station${offline.length === 1 ? "" : "s"}`;

  return (
    <div className="ticker" aria-live="polite">
      <span className={`tick ${online ? "is-good" : "is-bad"}`}>
        <span className="tick-dot" aria-hidden="true" />
        {headline}
      </span>
      {status !== null && (
        <>
          <span className="tick"><b>{status.pending}</b> queued</span>
          <span className={`tick${status.problems > 0 ? " is-warn" : ""}`}><b>{status.problems}</b> attention</span>
          {readers.length > 0 && <span className="tick is-bad">reader off ×{readers.length}</span>}
          {status.stations[0]?.event_name && <span className="tick tick-event">{status.stations[0].event_name}</span>}
        </>
      )}
      {status?.alarm && <span className="tick is-warn">{status.alarm}</span>}
      {errors.length > 0 && (
        <details className="station-errors">
          <summary>Station errors ({errors.length})</summary>
          <div className="station-error-list">
            {errors.map((station) => (
              <p className="station-error" key={station.id}>
                <strong>{station.name}</strong>: {station.last_error}
              </p>
            ))}
          </div>
        </details>
      )}
    </div>
  );
}

/** Headlines for failures `app_state` can report while starting the stations. */
const STARTUP_TITLES: Record<string, string> = {
  config_unreadable: "The saved setup cannot be read",
  invalid_setup: "The saved setup is not valid",
  settings_damaged: "The saved server settings cannot be read",
  no_storage: "RfiDex cannot open its data folder",
  no_reactor: "RfiDex could not start its stations",
};

function StartupProblem({
  title,
  detail,
  help,
  code,
  onRetry,
}: {
  title: string;
  detail: string;
  help?: ReactNode;
  code?: string;
  onRetry?: () => void;
}) {
  return (
    <div className="app">
      <main className="startup-problem" role="alert">
        <div className="brand-lockup">
          <span className="brand-mark" aria-hidden="true">
            r.
          </span>
          <p className="brand">RfiDex</p>
        </div>
        <div className="startup-layout">
        <div className="startup-art" aria-hidden="true">
          <svg viewBox="0 0 400 350" fill="none">
            <ellipse cx="200" cy="311" rx="151" ry="13" fill="var(--line-soft)" />
            <path d="M46 89h15m-7-7v15M346 219h16m-8-8v16" stroke="var(--muted)" strokeWidth="2" strokeLinecap="round" />
            <circle cx="337" cy="73" r="5" stroke="var(--muted)" strokeWidth="2" />
            <g transform="rotate(8 265 130)">
              <rect x="193" y="46" width="151" height="172" rx="13" fill="var(--bg-soft)" stroke="var(--muted)" strokeWidth="2" />
              <path d="M193 77h151" stroke="var(--muted)" strokeWidth="2" />
              <circle cx="209" cy="62" r="3" fill="var(--muted)" />
              <path d="M220 62h17" stroke="var(--muted)" strokeWidth="2" strokeLinecap="round" />
              <rect x="235" y="100" width="66" height="66" rx="13" fill="var(--accent)" />
              <text x="252" y="147" fill="white" fontSize="48" fontWeight="700" fontFamily="system-ui, sans-serif">r.</text>
              <path d="M248 187h39" stroke="var(--muted)" strokeWidth="5" strokeLinecap="round" />
            </g>
            <g transform="rotate(-7 159 205)">
              <path d="m118 277-9 29H88m109-29 10 29h21" stroke="var(--ink-2)" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" />
              <rect x="57" y="119" width="207" height="160" rx="14" fill="var(--surface)" stroke="var(--ink-2)" strokeWidth="2.5" />
              <path d="M58 151h205" stroke="var(--ink-2)" strokeWidth="2" />
              <circle cx="75" cy="135" r="3" fill="var(--warn-line)" /><circle cx="87" cy="135" r="3" fill="var(--line)" /><circle cx="99" cy="135" r="3" fill="var(--line)" />
              <rect x="117" y="130" width="123" height="10" rx="5" fill="var(--inset)" />
              <ellipse cx="130" cy="200" rx="5" ry="9" fill="var(--ink-2)" /><ellipse cx="185" cy="200" rx="5" ry="9" fill="var(--ink-2)" />
              <path d="M147 226q12-10 24 0" stroke="var(--ink-2)" strokeWidth="3" strokeLinecap="round" />
              <path d="m110 181 16-4m53 0 16 4" stroke="var(--ink-2)" strokeWidth="2" strokeLinecap="round" />
              <ellipse cx="110" cy="218" rx="10" ry="5" fill="var(--accent-dim)" /><ellipse cx="205" cy="218" rx="10" ry="5" fill="var(--accent-dim)" />
            </g>
            <path d="M62 207q-30-13-24-38m225 61q34 4 38-24" stroke="var(--ink-2)" strokeWidth="3" strokeLinecap="round" />
            <path d="m29 169 10-5 7 9" stroke="var(--ink-2)" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" />
            <path d="M115 82q25-30 58-20m-8-9 10 9-11 8" stroke="var(--warn-line)" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" strokeDasharray="5 5" />
          </svg>
          <span className="startup-art-caption">{help ? "Right place. Different window." : "A little help getting started."}</span>
        </div>
        <div className="startup-copy">
        <p className="eyebrow">{help ? "A small window mix-up" : "A pause before check-in"}</p>
        <h1>{title}</h1>
        <p className="startup-detail">{detail}</p>
        {help && <p className="startup-help">{help}</p>}
        {(onRetry || code) && <div className="startup-footer">
          {onRetry && (
            <button className="primary" type="button" onClick={onRetry}>
              Try again
            </button>
          )}
          {code && <span className="startup-code">Error code {code}</span>}
        </div>}
        </div>
        </div>
      </main>
    </div>
  );
}
