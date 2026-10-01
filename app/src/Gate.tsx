import { useEffect, useState } from "react";

import { errorText, formatTime, gateRecent } from "./api";
import type { GateView, StationStatus } from "./api";

interface Props {
  station: StationStatus;
}

/** How often the screen refreshes itself, after each answer rather than on a
 * fixed clock, so a slow reply never stacks requests up. */
const POLL_MS = 500;

export function Gate({ station }: Props) {
  const [rows, setRows] = useState<GateView[]>([]);
  const [failure, setFailure] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    let timer: number | undefined;
    const tick = async () => {
      try {
        const next = await gateRecent(station.id, 30);
        if (!live) return;
        setRows(next);
        setFailure(null);
      } catch (problem) {
        if (!live) return;
        setFailure(errorText(problem));
      }
      if (live) timer = window.setTimeout(tick, POLL_MS);
    };
    void tick();
    return () => {
      live = false;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [station.id]);

  const latest = rows[0] ?? null;
  const earlier = rows.slice(1);
  const accepted = rows.filter((row) => row.status === "accepted").length;
  const flagged = rows.filter((row) => row.status === "denied" || row.status === "problem").length;

  return (
    <div className="gate-console">
      <section
        className={`verdict verdict-stage gate-${latest?.status ?? "empty"}`}
        aria-live="polite"
      >
        <p className="eyebrow">
          Recording: {station.role === "exit" ? "Exit" : "Entry"}
        </p>
        <p className="status-word">{statusWord(latest)}</p>
        <h1>{latest ? latest.name ?? "Unknown sticker" : "Waiting"}</h1>
        <p className="message">{latest ? latest.message : "No passage yet."}</p>
        {latest && latest.anomalies.length > 0 && (
          <ul className="anomalies">
            {latest.anomalies.map((anomaly) => (
              <li key={anomaly}>{anomaly}</li>
            ))}
          </ul>
        )}
        <p className="timestamp">
          {latest ? `${latest.role === "exit" ? "Exit" : "Entry"} · Last passage at ${formatTime(latest.captured_at)}` : "—"}
        </p>
      </section>

      <div className="gate-feed">
        <dl className="gate-stats">
          <div><dt>Accepted</dt><dd>{accepted}</dd></div>
          <div><dt>Flagged</dt><dd className={flagged > 0 ? "is-warn" : ""}>{flagged}</dd></div>
          <div><dt>Total</dt><dd>{rows.length}</dd></div>
        </dl>
        <div className="recent" aria-label="Recent passages">
          <h2>Recent passages</h2>
          {earlier.length === 0 && <p className="empty">Nothing else yet.</p>}
          <ul>
            {earlier.map((row) => (
              <li key={`${station.id}:${row.id}`} className={`gate-${row.status}`}>
                <span className="time">{formatTime(row.captured_at)}<span className="passage-direction">{row.role === "exit" ? "Exit" : "Entry"}</span></span>
                <span className="who">{row.name ?? "Unknown sticker"}</span>
                <span className="what">{row.message}</span>
                {row.anomalies.length > 0 && (
                  <ul className="anomalies">
                    {row.anomalies.map((anomaly) => (
                      <li key={anomaly}>{anomaly}</li>
                    ))}
                  </ul>
                )}
              </li>
            ))}
          </ul>
        </div>
      </div>

      {failure && (
        <p className="failure" role="alert">
          {failure}
        </p>
      )}
    </div>
  );
}

/** The word, not only the colour: a recorded offline passage must never read
 * as accepted. */
function statusWord(view: GateView | null): string {
  switch (view?.status) {
    case "accepted":
      return "Accepted";
    case "denied":
      return "Denied";
    case "problem":
      return "Problem";
    case "recorded":
      return "Recorded — not sent yet";
    default:
      return "Ready";
  }
}
