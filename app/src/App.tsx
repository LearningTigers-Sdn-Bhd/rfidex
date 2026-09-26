import { useCallback, useEffect, useState } from "react";
import type { ReactNode } from "react";

import { appState, errorText, exportDiagnostics, failureOf, inDesktopApp, status, syncNow } from "./api";
import type { AppFailure, AppStatus, AppView, StationStatus } from "./api";
import { Desk } from "./Desk";
import { Gate } from "./Gate";
import { Problems } from "./Problems";
import { Setup } from "./Setup";
import { Simulator } from "./Simulator";

/** The status bar refreshes after each answer, never on an overlapping clock. */
const STATUS_MS = 1000;

type Tab = "station" | "problems";

export function App() {
  const [view, setView] = useState<AppView | null>(null);
  const [fatal, setFatal] = useState<AppFailure | null>(null);
  const [showSetup, setShowSetup] = useState(false);
  const [tab, setTab] = useState<Tab>("station");
  const [selected, setSelected] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [exportPath, setExportPath] = useState<string | null>(null);

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

  if (!view.configured || showSetup) {
    return (
      <div className="app">
        <main className="workspace">
          <Setup
            hasSavedConfig={view.configured}
            status={view.status}
            onSaved={(saved) => {
              setView(saved);
              setShowSetup(false);
              setNotice("Setup saved.");
            }}
            onCancel={() => setShowSetup(false)}
          />
        </main>
      </div>
    );
  }

  const stations = view.status?.stations ?? [];
  // A station that disappeared after a save must not leave the screen blank.
  const station: StationStatus | null =
    stations.find((candidate) => candidate.id === selected) ?? stations[0] ?? null;

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

  const runExport = async () => {
    setBusy(true);
    setNotice(null);
    setExportPath(null);
    try {
      setExportPath(await exportDiagnostics());
    } catch (problem) {
      setNotice(errorText(problem));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand-lockup"><span className="brand-mark" aria-hidden="true">r.</span><h1 className="brand">RfiDex<span>Event operations</span></h1></div>
        <nav aria-label="Sections">
          <button
            type="button"
            className={tab === "station" ? "tab is-current" : "tab"}
            aria-current={tab === "station" ? "page" : undefined}
            onClick={() => setTab("station")}
          >
            Stations
          </button>
          <button
            type="button"
            className={tab === "problems" ? "tab is-current" : "tab"}
            aria-current={tab === "problems" ? "page" : undefined}
            onClick={() => setTab("problems")}
          >
            Problems ({view.status?.problems ?? 0})
          </button>
        </nav>
        <div className="topbar-actions">
          <button type="button" onClick={() => setShowSetup(true)}>
            Setup
          </button>
          <button type="button" onClick={() => void runSync()} disabled={busy}>
            {busy ? "Working…" : "Sync now"}
          </button>
          <button type="button" onClick={() => void runExport()} disabled={busy}>
            Export diagnostics
          </button>
        </div>
      </header>

      <main className="workspace">
        {stations.length === 0 ? (
          <section className="empty-state">
            <h2>No stations on this computer yet</h2>
            <p>Open Setup and add the desk and gates this PC runs.</p>
            <button type="button" onClick={() => setShowSetup(true)}>
              Open Setup
            </button>
          </section>
        ) : tab === "problems" ? (
          <Problems problemCount={view.status?.problems ?? 0} />
        ) : (
          <>
            <nav className="station-tabs" aria-label="Stations">
              {stations.map((candidate) => (
                <button
                  key={candidate.id}
                  type="button"
                  className={candidate.id === station?.id ? "tab is-current" : "tab"}
                  aria-current={candidate.id === station?.id ? "true" : undefined}
                  onClick={() => setSelected(candidate.id)}
                >
                  <span className="station-kind">{candidate.kind === "desk" ? "Desk" : candidate.role === "exit" ? "Exit gate" : "Entry gate"}</span>
                  <span className="station-name">{candidate.name}</span>
                  {!candidate.connected && <span className="badge">reader off</span>}
                </button>
              ))}
            </nav>

            {station && (
              // Keyed: a station switch remounts its screens, so a request still
              // in flight for the old station can never land on the new one.
              <div className="station" key={station.id}>
                <div className="station-heading">
                  <div><p className="eyebrow">{station.kind === "desk" ? "Registration" : "Access control"}</p><h2>{station.name}</h2></div>
                  <div className="station-indicators"><span className={station.connected ? "connection" : "connection disconnected"}>{station.connected ? "Reader connected" : "Reader disconnected"}</span>{station.simulated && <span className="simulation-label">Simulated</span>}</div>
                </div>
                <div className="station-body">
                  {station.kind === "desk" ? (
                    <Desk station={station} />
                  ) : (
                    <Gate station={station} />
                  )}
                </div>
                <Simulator station={station} />
              </div>
            )}
          </>
        )}

        {notice && <p className="note">{notice}</p>}
        {exportPath && (
          <p className="note">
            Diagnostics written to <code className="path">{exportPath}</code>
          </p>
        )}
      </main>

      <StatusBar status={view.status} />
    </div>
  );
}

function StatusBar({ status }: { status: AppStatus | null }) {
  if (!status) {
    return (
      <footer className="status-bar" aria-live="polite">
        <p>Waiting for the first status.</p>
      </footer>
    );
  }
  const offline = status.stations.filter((station) => !station.online);
  const readers = status.stations.filter((station) => !station.connected);
  return (
    <footer className={`status-bar${offline.length ? " has-offline" : ""}`} aria-live="polite">
      <p>
        <strong>
          {offline.length === 0 ? "Online" : `Offline at ${offline.length} station(s)`}
        </strong>
        <span> · {status.pending} waiting to send</span>
        <span> · {status.problems} needing attention</span>
        {status.stations[0]?.event_name && (
          <span> · {status.stations[0].event_name}</span>
        )}
        {readers.length > 0 && (
          <span className="bad"> · reader disconnected at {readers.length}</span>
        )}
      </p>
      {status.alarm && <p className="alarm">{status.alarm}</p>}
      {status.stations
        .filter((station) => station.last_error)
        .map((station) => (
          <p className="station-error" key={station.id}>
            <strong>{station.name}</strong>: {station.last_error}
          </p>
        ))}
    </footer>
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
          <span className="brand-mark" aria-hidden="true">r.</span>
          <p className="brand">RfiDex</p>
        </div>
        <div className="startup-layout">
        <div className="startup-art" aria-hidden="true">
          <svg viewBox="0 0 400 350" fill="none">
            <ellipse cx="200" cy="311" rx="151" ry="13" fill="#e3e8dc" />
            <path d="M46 89h15m-7-7v15M346 219h16m-8-8v16" stroke="#97a88b" strokeWidth="2" strokeLinecap="round" />
            <circle cx="337" cy="73" r="5" stroke="#97a88b" strokeWidth="2" />
            <g transform="rotate(8 265 130)">
              <rect x="193" y="46" width="151" height="172" rx="13" fill="#e2eadb" stroke="#59715a" strokeWidth="2" />
              <path d="M193 77h151" stroke="#59715a" strokeWidth="2" />
              <circle cx="209" cy="62" r="3" fill="#59715a" />
              <path d="M220 62h17" stroke="#59715a" strokeWidth="2" strokeLinecap="round" />
              <rect x="235" y="100" width="66" height="66" rx="13" fill="#245b43" />
              <text x="252" y="147" fill="white" fontSize="48" fontWeight="700" fontFamily="system-ui, sans-serif">r.</text>
              <path d="M248 187h39" stroke="#8fa181" strokeWidth="5" strokeLinecap="round" />
            </g>
            <g transform="rotate(-7 159 205)">
              <path d="m118 277-9 29H88m109-29 10 29h21" stroke="#354d3c" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" />
              <rect x="57" y="119" width="207" height="160" rx="14" fill="#fffef9" stroke="#354d3c" strokeWidth="2.5" />
              <path d="M58 151h205" stroke="#354d3c" strokeWidth="2" />
              <circle cx="75" cy="135" r="3" fill="#b98049" /><circle cx="87" cy="135" r="3" fill="#b7c5a8" /><circle cx="99" cy="135" r="3" fill="#b7c5a8" />
              <rect x="117" y="130" width="123" height="10" rx="5" fill="#edf0e7" />
              <ellipse cx="130" cy="200" rx="5" ry="9" fill="#354d3c" /><ellipse cx="185" cy="200" rx="5" ry="9" fill="#354d3c" />
              <path d="M147 226q12-10 24 0" stroke="#354d3c" strokeWidth="3" strokeLinecap="round" />
              <path d="m110 181 16-4m53 0 16 4" stroke="#354d3c" strokeWidth="2" strokeLinecap="round" />
              <ellipse cx="110" cy="218" rx="10" ry="5" fill="#f0d9be" /><ellipse cx="205" cy="218" rx="10" ry="5" fill="#f0d9be" />
            </g>
            <path d="M62 207q-30-13-24-38m225 61q34 4 38-24" stroke="#354d3c" strokeWidth="3" strokeLinecap="round" />
            <path d="m29 169 10-5 7 9" stroke="#354d3c" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" />
            <path d="M115 82q25-30 58-20m-8-9 10 9-11 8" stroke="#a7753e" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" strokeDasharray="5 5" />
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
