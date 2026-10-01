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
      <div className="gate-overview">
        <div className="gate-overview-heading">
          <h2>Gate monitor</h2>
          <p>Latest {rows.length} passages · up to 30 records</p>
        </div>
        <dl className="gate-stats">
          <div><dt>Accepted</dt><dd>{accepted}</dd></div>
          <div><dt>Flagged</dt><dd className={flagged > 0 ? "is-warn" : ""}>{flagged}</dd></div>
          <div><dt>Total</dt><dd>{rows.length}</dd></div>
        </dl>
      </div>

      <section
        className={`verdict gate-passage gate-${latest?.status ?? "empty"}`}
        aria-live="polite"
      >
        <div className="gate-passage-head">
          <span className="eyebrow">Latest passage</span>
          <span className="gate-role">Recording {station.role === "exit" ? "exit" : "entry"} ↗</span>
        </div>
        <div className="gate-passage-body">
          <div className="gate-outcome">
            <div className="desk-status-icon" aria-hidden="true">
              <svg viewBox="0 0 48 48" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                {latest?.status === "accepted" ? <path d="m13 24 8 8 15-16" />
                  : latest?.status === "denied" ? <path d="m15 15 18 18M33 15 15 33" />
                  : latest?.status === "recorded" ? <><circle cx="24" cy="24" r="15" /><path d="M24 15v10l7 4" /></>
                  : latest?.status === "problem" ? <><path d="M24 13v14" /><circle cx="24" cy="34" r="1" /></>
                  : <><path d="M13 10h18v28H13M23 24h17m-6-6 6 6-6 6" /></>}
              </svg>
            </div>
            <p className="status-word">{statusWord(latest)}</p>
          </div>
          <div className="gate-identity">
            <h1>{latest ? latest.name ?? "Unknown sticker" : "Waiting for a guest"}</h1>
            <p className="message">{latest ? latest.message : "No passage yet."}</p>
            {latest && latest.anomalies.length > 0 && (
              <ul className="anomalies">
                {latest.anomalies.map((anomaly) => <li key={anomaly}>{anomaly}</li>)}
              </ul>
            )}
          </div>
        </div>
        <div className="gate-passage-footer">
          <span>{latest ? `${latest.role === "exit" ? "Exit" : "Entry"} passage` : "Awaiting a sticker"}</span>
          {latest && <time dateTime={latest.captured_at}>{formatTime(latest.captured_at)}</time>}
        </div>
      </section>

      <section className="recent" aria-label="Recent passages">
        <div className="gate-history-heading">
          <h2>Recent passages</h2>
          <span>Previous reads</span>
        </div>
        {earlier.length === 0 && <p className="empty">Nothing else yet.</p>}
        <ul>
          {earlier.map((row) => (
            <li key={`${station.id}:${row.id}`} className={`gate-${row.status}`}>
              <div className="passage-guest">
                <span className="who">{row.name ?? "Unknown sticker"}</span>
                <span className="what">{row.message}</span>
                {row.anomalies.length > 0 && (
                  <ul className="anomalies">
                    {row.anomalies.map((anomaly) => <li key={anomaly}>{anomaly}</li>)}
                  </ul>
                )}
              </div>
              <span className="passage-status">{statusWord(row)}</span>
              <span className="passage-direction">{row.role === "exit" ? "Exit ↗" : "Entry ↘"}</span>
              <time className="time" dateTime={row.captured_at}>{formatTime(row.captured_at)}</time>
            </li>
          ))}
        </ul>
      </section>

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
