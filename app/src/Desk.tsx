import { useCallback, useEffect, useRef, useState } from "react";
import type { FormEvent } from "react";

import { deskLink, deskReset, deskScan, errorText } from "./api";
import type { DeskView, StationStatus } from "./api";

interface Props {
  station: StationStatus;
}

export function Desk({ station }: Props) {
  const [view, setView] = useState<DeskView | null>(null);
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const [reason, setReason] = useState("");

  // Every scan bumps this, so a timer that started under an older scan can
  // never replace the newer view. A station switch remounts this component.
  const generation = useRef(0);
  const qrRef = useRef<HTMLInputElement>(null);
  const dialogRef = useRef<HTMLDialogElement>(null);

  const run = useCallback(async (operation: () => Promise<DeskView>) => {
    setBusy(true);
    try {
      const next = await operation();
      setView(next);
      setFailure(null);
      return next;
    } catch (problem) {
      setFailure(errorText(problem));
      return null;
    } finally {
      setBusy(false);
    }
  }, []);

  const submitScan = (event: FormEvent) => {
    event.preventDefault();
    const trimmed = code.trim();
    if (!trimmed || busy) return;
    generation.current += 1;
    setCode("");
    setReason("");
    void run(() => deskScan(station.id, trimmed));
  };

  // A scanned ticket goes straight to the reader: in the normal flow the
  // sticker is already there, and the runtime refuses cleanly when it is not.
  useEffect(() => {
    if (view?.step !== "scanned" || busy) return;
    void run(() => deskLink(station.id, null));
  }, [view, busy, run, station.id]);

  // Waiting for a sticker retries by itself; a finished link clears itself.
  useEffect(() => {
    const waiting = view?.step === "error" && view.code === "no_tag";
    const linked = view?.step === "linked";
    if (!waiting && !linked) return;
    const mine = generation.current;
    const timer = window.setTimeout(
      () => {
        if (generation.current !== mine) return;
        void run(() =>
          linked ? deskReset(station.id) : deskLink(station.id, null),
        ).then((next) => {
          if (next && linked && generation.current === mine) qrRef.current?.focus();
        });
      },
      linked ? 3000 : 500,
    );
    return () => window.clearTimeout(timer);
  }, [view, run, station.id]);

  const confirming = view?.step === "needs_confirm";

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (confirming && !dialog.open) {
      setReason("");
      dialog.showModal();
    } else if (!confirming && dialog.open) {
      dialog.close();
    }
  }, [confirming]);

  // The scanner types into the code field, so it keeps focus unless the
  // operator is answering a confirmation.
  useEffect(() => {
    if (confirming) return;
    qrRef.current?.focus();
  }, [confirming, view?.step, station.id]);

  const confirm = (event: FormEvent) => {
    // Keep the dialog open when the reason is blank: a replacement always has
    // a reason, and the runtime would refuse it anyway.
    event.preventDefault();
    if (busy || reason.trim() === "") return;
    void run(() => deskLink(station.id, reason.trim()));
  };

  const step = view?.step ?? "ready";

  return (
    <div className="station-layout">
      <div className="station-main">
        <form className="scan" onSubmit={submitScan}>
          <label htmlFor="ticket-code">Ticket code</label>
          <input
            id="ticket-code"
            ref={qrRef}
            value={code}
            onChange={(event) => setCode(event.target.value)}
            autoComplete="off"
            placeholder="Scan the ticket QR code"
            disabled={busy}
          />
          <button type="submit" disabled={busy || code.trim() === ""}>
            {busy ? "Working…" : "Scan ticket"}
          </button>
        </form>

        <section
          className={`result desk-${step}${view?.offline ? " is-offline" : ""}`}
          aria-live="polite"
        >
          <p className="eyebrow">
            {view ? `Mode: ${view.mode === "write" ? "Write" : "Bind"}` : "Mode: —"}
          </p>
          <h1>{headline(view)}</h1>
          {view?.ticket && (
            <p className="ticket">
              <strong>{view.ticket.name}</strong>
              <span> · {view.ticket.ticket_type}</span>
            </p>
          )}
          <p className="message">{view ? view.message : "Scan a ticket to begin."}</p>
          {view?.offline && (
            <p className="offline-note" role="status">
              Offline — badge will print when connection returns.
            </p>
          )}
          <div className="actions">
            <button
              type="button"
              onClick={() => void run(() => deskLink(station.id, null))}
              disabled={busy || !view?.ticket || confirming}
            >
              Try the sticker again
            </button>
            <button
              type="button"
              onClick={() => void run(() => deskReset(station.id))}
              disabled={busy || confirming}
            >
              Cancel and start over
            </button>
          </div>
        </section>

        {failure && (
          <p className="failure" role="alert">
            {failure}
          </p>
        )}
      </div>

      <dialog
        ref={dialogRef}
        className="confirm"
        onCancel={() => {
          setReason("");
          void run(() => deskReset(station.id));
        }}
      >
        <h2>Replace this sticker?</h2>
        <p className="message">{view?.message}</p>
        {view?.ticket && (
          <p className="ticket">
            <strong>{view.ticket.name}</strong>
            <span> · {view.ticket.ticket_type}</span>
          </p>
        )}
        <form onSubmit={confirm}>
          <label htmlFor="reason">Reason for the replacement (required)</label>
          <input
            id="reason"
            value={reason}
            onChange={(event) => setReason(event.target.value)}
            autoComplete="off"
          />
          <div className="dialog-actions">
            <button
              type="button"
              onClick={() => {
                setReason("");
                void run(() => deskReset(station.id));
              }}
            >
              Cancel — do not replace
            </button>
            <button type="submit" disabled={busy || reason.trim() === ""}>
              Replace sticker
            </button>
          </div>
        </form>
      </dialog>
    </div>
  );
}

function headline(view: DeskView | null): string {
  switch (view?.step) {
    case "scanned":
      return "Ticket found";
    case "linked":
      return "Linked";
    case "needs_confirm":
      return "Needs confirmation";
    case "error":
      return "Not yet";
    default:
      return "Ready";
  }
}
