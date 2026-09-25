import { useCallback, useEffect, useState } from "react";

import { appState, errorText, exportDiagnostics, status, syncNow } from "./api";
import type { AppStatus, AppView, StationStatus } from "./api";
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
  const [fatal, setFatal] = useState<string | null>(null);
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
      setFatal(errorText(problem));
      return null;
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

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

  if (fatal) {
    return (
      <div className="app">
        <main className="workspace">
          <p className="failure" role="alert">
            {fatal}
          </p>
        </main>
      </div>
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
        <h1 className="brand">RfiDex</h1>
        <nav aria-label="Sections">
          <button
            type="button"
            className={tab === "station" ? "tab is-current" : "tab"}
            onClick={() => setTab("station")}
          >
            Stations
          </button>
          <button
            type="button"
            className={tab === "problems" ? "tab is-current" : "tab"}
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
                  onClick={() => setSelected(candidate.id)}
                >
                  {candidate.name}
                  {!candidate.connected && <span className="badge">reader off</span>}
                </button>
              ))}
            </nav>

            {station && (
              <div className="station">
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
    <footer className="status-bar" aria-live="polite">
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
