import { useCallback, useEffect, useRef, useState } from "react";

import { dismissProblem, errorText, formatTime, problems } from "./api";
import type { ProblemView } from "./api";

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
    <div className="panel">
      <div className="panel-head">
        <div><p className="eyebrow">Review queue</p><h1>Problems</h1></div>
        <button type="button" onClick={() => void refresh()} disabled={busy}>
          Refresh
        </button>
      </div>

      {failure && (
        <p className="failure" role="alert">
          {failure}
        </p>
      )}

      {items.length === 0 ? (
        <p className="empty">Nothing needs attention.</p>
      ) : (
        <ul className="problems">
          {items.map((problem) => (
            <li key={`${problem.station_id}:${problem.id}`}>
              <p className="who">
                <strong>{problem.name ?? "Unknown sticker"}</strong>
                <span> · {problem.station_name}</span>
                <span className="time"> · {formatTime(problem.captured_at)}</span>
              </p>
              <p className="message">{problem.message}</p>
              <button type="button" onClick={() => setChoosing(problem)} disabled={busy}>
                Dismiss from this list
              </button>
            </li>
          ))}
        </ul>
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
