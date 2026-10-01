import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { dismissProblem, errorText, formatTime, problems } from "./api";
import type { ProblemView } from "./api";
import { problemPage } from "./problem-list";

interface Props {
  /** The status problem count: when it changes, this list must catch up. */
  problemCount: number;
}

export function Problems({ problemCount }: Props) {
  const [items, setItems] = useState<ProblemView[]>([]);
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [choosing, setChoosing] = useState<ProblemView | null>(null);
  const dialogRef = useRef<HTMLDialogElement>(null);
  const tableRef = useRef<HTMLDivElement>(null);
  const [query, setQuery] = useState("");
  const [page, setPage] = useState(0);
  const result = useMemo(() => problemPage(items, query, page), [items, query, page]);

  useEffect(() => {
    setPage(result.page);
    tableRef.current?.scrollTo({ top: 0 });
  }, [result.page, query]);

  const refresh = useCallback(async () => {
    try {
      setItems(await problems());
      setFailure(null);
    } catch (problem) {
      setFailure(errorText(problem));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh, problemCount]);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (choosing && !dialog.open) dialog.showModal();
    else if (!choosing && dialog.open) dialog.close();
  }, [choosing]);

  const dismiss = async (problem: ProblemView) => {
    setBusy(true);
    try {
      await dismissProblem(problem.station_id, problem.id);
      setChoosing(null);
      await refresh();
    } catch (problemFailure) {
      setFailure(errorText(problemFailure));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="panel problems-page">
      <header className="panel-head">
        <div>
          <h1 id="problems-title">Problems</h1>
          <p className="problems-intro">Scan and sync records that need a staff review.</p>
        </div>
        <button type="button" className="problems-refresh" onClick={() => void refresh()} disabled={busy}>
          <svg className="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M20 11a8 8 0 0 0-14.9-3M4 13a8 8 0 0 0 14.9 3M4 4v4h4M20 20v-4h-4" />
          </svg>
          Refresh
        </button>
      </header>

      <div className="problems-search">
        <label htmlFor="problem-search">Search problems</label>
        <input id="problem-search" type="search" placeholder="Guest, station or issue" value={query}
          onChange={(event) => { setQuery(event.target.value); setPage(0); }} />
        {query && <button type="button" onClick={() => { setQuery(""); setPage(0); }}>Clear search</button>}
      </div>
      <div className="problems-summary">
        <span aria-live="polite"><strong>{result.total}</strong>{query.trim() ? ` of ${items.length}` : ""} to review</span>
        <p>Dismissing an item hides it here. Its saved record stays intact.</p>
      </div>

      {failure && (
        <p className="failure" role="alert">
          {failure}
        </p>
      )}

      {items.length === 0 ? (
        <div className="problems-empty">
          <h2>Nothing needs attention</h2>
          <p>Any scan or sync issue that needs review will appear here.</p>
        </div>
      ) : result.total === 0 ? (
        <div className="problems-empty">
          <h2>No matching problems</h2>
          <p>Try another guest name, station or issue, or clear the search.</p>
        </div>
      ) : (
        <div ref={tableRef} className="problems-table-wrap" role="region" aria-labelledby="problems-title" tabIndex={0}>
          <table className="problems-table" aria-label="Issues needing review">
            <colgroup>
              <col className="problem-time-col" />
              <col className="problem-guest-col" />
              <col className="problem-station-col" />
              <col />
              <col className="problem-action-col" />
            </colgroup>
            <thead>
              <tr>
                <th scope="col">Time</th>
                <th scope="col">Guest</th>
                <th scope="col">Station</th>
                <th scope="col">What happened</th>
                <th scope="col" className="problem-action">Action</th>
              </tr>
            </thead>
            <tbody>
              {result.rows.map((problem) => (
                <tr key={`${problem.station_id}:${problem.id}`}>
                  <td className="problem-time"><time dateTime={problem.captured_at}>{formatTime(problem.captured_at)}</time></td>
                  <th scope="row">{problem.name ?? "Unknown sticker"}</th>
                  <td className="problem-station">{problem.station_name}</td>
                  <td>{problem.message}</td>
                  <td className="problem-action">
                    <button
                      type="button"
                      className="problem-dismiss"
                      title="Dismiss from list"
                      aria-label={`Dismiss issue for ${problem.name ?? "unknown sticker"} at ${problem.station_name}`}
                      onClick={() => setChoosing(problem)}
                      disabled={busy}
                    >
                      Dismiss
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {result.total > 0 && (
        <nav className="problems-pagination" aria-label="Problem list pages">
          <span>{result.start + 1}–{result.start + result.rows.length} of {result.total}</span>
          <div>
            <button type="button" disabled={result.page === 0} onClick={() => setPage(result.page - 1)}>Previous</button>
            <span>Page {result.page + 1} of {result.pages}</span>
            <button type="button" disabled={result.page + 1 === result.pages} onClick={() => setPage(result.page + 1)}>Next</button>
          </div>
        </nav>
      )}

      <dialog ref={dialogRef} className="confirm" aria-labelledby="dismiss-title" onCancel={() => setChoosing(null)}>
        <h2 id="dismiss-title">Dismiss this from the list?</h2>
        <p>
          This hides it here. It does <strong>not</strong> fix the server&rsquo;s
          answer, delete the saved action, or change who holds the sticker.
        </p>
        {choosing && (
          <p className="message">
            <strong>{choosing.name ?? "Unknown sticker"}</strong> — {choosing.message}
          </p>
        )}
        <div className="dialog-actions">
          <button type="button" onClick={() => setChoosing(null)}>
            Keep it in the list
          </button>
          <button
            type="button"
            onClick={() => choosing && void dismiss(choosing)}
            disabled={busy || !choosing}
          >
            Dismiss from this list
          </button>
        </div>
      </dialog>
    </div>
  );
}
