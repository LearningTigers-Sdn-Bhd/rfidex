import { getCurrentWindow } from "@tauri-apps/api/window";
import { useCallback, useEffect, useState } from "react";

import { gateRecent } from "./api";
import type { GateView, StationStatus } from "./api";

const POLL_MS = 1000;

const entryTime = new Intl.DateTimeFormat("en-MY", {
  timeZone: "Asia/Kuching",
  hour: "2-digit",
  minute: "2-digit",
  hour12: true,
});

/** People-facing wall screen, laid out like the EventzFlow arrival display:
 * the newest guest large, nine more beside, then three columns of ten. Only
 * guests who passed are shown: no sync state, denials or warnings. */
export function GateDisplay({ station, onClose }: { station: StationStatus; onClose: () => void }) {
  const [rows, setRows] = useState<GateView[]>([]);
  const [online, setOnline] = useState(false);
  const [full, setFull] = useState(false);

  useEffect(() => {
    let live = true;
    let timer: number | undefined;
    const tick = async () => {
      try {
        const next = await gateRecent(station.id, 100);
        if (!live) return;
        setRows(next.filter((row) => (row.status === "accepted" || row.status === "recorded") && row.name));
        setOnline(true);
      } catch {
        if (live) setOnline(false);
      }
      if (live) timer = window.setTimeout(tick, POLL_MS);
    };
    void tick();
    return () => {
      live = false;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [station.id]);

  // The native window goes full screen, not just the page: a webview's own
  // fullscreen is unreliable. Outside the app (browser preview) this is a no-op.
  const setFullscreen = useCallback(async (on: boolean) => {
    try {
      await getCurrentWindow().setFullscreen(on);
      setFull(on);
    } catch {
      // No window bridge (browser preview): use the page's own fullscreen.
      try {
        if (on) await document.documentElement.requestFullscreen();
        else if (document.fullscreenElement) await document.exitFullscreen();
        setFull(on);
      } catch {
        // Refused: the overlay already fills the window.
      }
    }
  }, []);

  useEffect(() => {
    void setFullscreen(true);
    return () => void setFullscreen(false);
  }, [setFullscreen]);

  // The browser's own Esc leaves fullscreen without going through us.
  useEffect(() => {
    const sync = () => !document.fullscreenElement && setFull(false);
    document.addEventListener("fullscreenchange", sync);
    return () => document.removeEventListener("fullscreenchange", sync);
  }, []);

  // Esc leaves full screen first; a second Esc closes the display.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (full) void setFullscreen(false);
      else onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [full, onClose, setFullscreen]);

  const exit = station.role === "exit";
  const noun = exit ? "departures" : "arrivals";
  const shown = rows.filter((row) => row.role === station.role).slice(0, 40);

  const item = (row: GateView, main: boolean, newest = false) => (
    <li key={row.id} className={main ? "gd-card" : "gd-row"} data-newest={newest || undefined} data-direction={row.role}>
      <span className="gd-name">{row.name}</span>
      <span className="gd-meta">
        <span className="gd-dir">{row.role === "exit" ? "EXIT" : "ENTRY"}</span>
        <time dateTime={row.captured_at}>{entryTime.format(new Date(row.captured_at)).toUpperCase()}</time>
      </span>
    </li>
  );

  return (
    <div className="gd-screen" data-mode={exit ? "out" : "in"}>
      <header className="gd-header">
        <h1>{station.event_name ?? "Event"}</h1>
        <p>{exit ? "Departures" : "Welcome"}</p>
      </header>
      <section className="gd-latest">
        <div className="gd-heading"><h2>Latest {noun}</h2><span>{exit ? "Exit time" : "Entry time"}</span></div>
        {shown.length ? (
          <ol className="gd-cards">{shown.slice(0, 10).map((row, i) => item(row, true, i === 0))}</ol>
        ) : (
          <div className="gd-empty">
            <h3>{online ? (exit ? "Waiting for gate activity" : "Ready to welcome you") : "Connecting to the gate"}</h3>
            <p>{exit ? "Names will appear here when guests pass the gate." : "Your name will appear here as you enter."}</p>
          </div>
        )}
      </section>
      <section className="gd-previous">
        <div className="gd-heading"><h2>Earlier {noun}</h2></div>
        <div className="gd-columns">
          {[0, 1, 2].map((column) => (
            <ol key={column} className="gd-history" start={11 + column * 10}>
              {shown.slice(10 + column * 10, 20 + column * 10).map((row) => item(row, false))}
            </ol>
          ))}
        </div>
      </section>
      <footer className="gd-footer">
        <span>Malaysia time · UTC+8</span>
        <span className="gd-actions">
          <button type="button" onClick={() => void setFullscreen(!full)}>{full ? "Exit full screen" : "Full screen"}</button>
          <button type="button" onClick={onClose}>Close display</button>
        </span>
      </footer>
    </div>
  );
}
