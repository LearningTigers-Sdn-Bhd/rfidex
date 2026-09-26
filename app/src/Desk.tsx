import { useCallback, useEffect, useRef, useState } from "react";
import type { FormEvent } from "react";

import {
  deskLink,
  deskPrint,
  deskReset,
  deskScan,
  deskSearch,
  errorText,
} from "./api";
import type { BadgeView, DeskView, SearchBy, SearchView, StationStatus } from "./api";

interface Props {
  station: StationStatus;
}

const SEARCH_DELAY_MS = 300;
const MODES: SearchBy[] = ["name", "email", "phone"];
const MODE_LABELS: Record<SearchBy, string> = {
  name: "Name",
  email: "Email",
  phone: "Phone",
};

export function Desk({ station }: Props) {
  const [view, setView] = useState<DeskView | null>(null);
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const [reason, setReason] = useState("");
  const [printing, setPrinting] = useState(false);
  // A print answer is only shown for the guest it was asked about.
  const [printed, setPrinted] = useState<{ sessionId: string; badge: BadgeView } | null>(null);

  const [searchOpen, setSearchOpen] = useState(false);
  const [by, setBy] = useState<SearchBy>("name");
  const [query, setQuery] = useState("");
  const [found, setFound] = useState<SearchView | null>(null);

  // Every scan bumps this, so a timer that started under an older scan can
  // never replace the newer view. A station switch remounts this component.
  const generation = useRef(0);
  // The same idea for search: only the newest answer may be shown.
  const asked = useRef(0);
  const qrRef = useRef<HTMLInputElement>(null);
  const searchRef = useRef<HTMLDivElement>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const dialogRef = useRef<HTMLDialogElement>(null);

  // A print answer is shown only while it belongs to the guest on screen.
  const badge =
    printed && printed.sessionId === view?.session_id ? printed.badge : view?.badge;

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

  const startScan = useCallback(
    (ticketCode: string) => {
      generation.current += 1;
      setCode("");
      setReason("");
      void run(() => deskScan(station.id, ticketCode));
    },
    [run, station.id],
  );

  const submitScan = (event: FormEvent) => {
    event.preventDefault();
    const trimmed = code.trim();
    if (!trimmed || busy) return;
    startScan(trimmed);
  };

  // A scanned ticket goes straight to the reader: in the normal flow the
  // sticker is already there, and the runtime refuses cleanly when it is not.
  useEffect(() => {
    if (view?.step !== "scanned" || busy) return;
    void run(() => deskLink(station.id, null));
  }, [view, busy, run, station.id]);

  // Waiting for a sticker retries by itself; a finished link clears itself
  // unless the badge still needs a person.
  useEffect(() => {
    const waiting = view?.step === "error" && view.code === "no_tag";
    const linked = view?.step === "linked" && !badge?.hold;
    if (!waiting && !linked) return;
    const mine = generation.current;
    const timer = window.setTimeout(
      () => {
        if (generation.current !== mine) return;
        void run(() =>
          linked ? deskReset(station.id) : deskLink(station.id, null),
        ).then((next) => {
          if (next && linked && generation.current === mine && !document.querySelector("dialog[open]")) qrRef.current?.focus();
        });
      },
      linked ? 3000 : 500,
    );
    return () => window.clearTimeout(timer);
  }, [view, busy, run, station.id, badge?.hold]);

  const ask = useCallback(() => {
    const mine = ++asked.current;
    void deskSearch(station.id, by, query.trim())
      .then((next) => {
        if (asked.current !== mine) return;
        setFound(next);
        setFailure(null);
      })
      .catch((problem) => {
        if (asked.current !== mine) return;
        setFound(null);
        setFailure(errorText(problem));
      });
  }, [station.id, by, query]);

  // Wait for the typing to stop, then ask once. The counter above drops any
  // answer that is older than the latest input.
  useEffect(() => {
    if (!searchOpen) return;
    const timer = window.setTimeout(ask, SEARCH_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [searchOpen, ask]);

  const openSearch = () => {
    setSearchOpen(true);
    setFound(null);
  };

  const closeSearch = () => {
    asked.current += 1;
    setSearchOpen(false);
    setQuery("");
    setFound(null);
    qrRef.current?.focus();
  };

  const pick = (publicId: string) => {
    closeSearch();
    startScan(publicId);
  };

  const print = useCallback(
    (sessionId: string) => {
      setPrinting(true);
      void deskPrint(station.id, sessionId)
        .then((badge) => setPrinted({ sessionId, badge }))
        .catch((problem) => setFailure(errorText(problem)))
        .finally(() => setPrinting(false));
    },
    [station.id],
  );

  // A first check-in asks for its own print, without waiting in the sticker
  // path: linking a sticker never depends on the printer. The dependencies are
  // the session and the signal alone, so a view update mid-print cannot ask for
  // a second job.
  useEffect(() => {
    if (!view?.badge.print_now) return;
    print(view.session_id);
  }, [view?.session_id, view?.badge.print_now, print]);

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

  // Keep the keyboard-wedge target focused outside native modal dialogs and
  // outside the search region, which the operator is using deliberately.
  useEffect(() => {
    let timer: number | undefined;
    const restore = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        if (document.querySelector("dialog[open]")) return;
        const active = document.activeElement;
        if (active && searchRef.current?.contains(active)) return;
        qrRef.current?.focus();
      }, 0);
    };
    const observer = new MutationObserver(restore);
    document.querySelectorAll("dialog").forEach((dialog) =>
      observer.observe(dialog, { attributes: true, attributeFilter: ["open"] }),
    );
    document.addEventListener("focusin", restore);
    document.addEventListener("pointerup", restore);
    window.addEventListener("focus", restore);
    restore();
    return () => {
      window.clearTimeout(timer);
      observer.disconnect();
      document.removeEventListener("focusin", restore);
      document.removeEventListener("pointerup", restore);
      window.removeEventListener("focus", restore);
    };
  }, []);

  const confirm = (event: FormEvent) => {
    // Keep the dialog open when the reason is blank: a replacement always has
    // a reason, and the runtime would refuse it anyway.
    event.preventDefault();
    if (busy || reason.trim() === "") return;
    void run(() => deskLink(station.id, reason.trim()));
  };

  useEffect(() => {
    if (searchOpen) searchInputRef.current?.focus();
  }, [searchOpen]);

  const step = view?.step ?? "ready";

  return (
    <div className="station-layout desk-layout">
      <div className="station-main">
        <form className="scan" onSubmit={submitScan}>
          <label htmlFor="ticket-code">Ticket code</label>
          <span className="scan-hint" id="scan-help">Scan a QR code or type a ticket code, then press Enter.</span>
          <input
            id="ticket-code"
            ref={qrRef}
            value={code}
            onChange={(event) => setCode(event.target.value)}
            autoComplete="off"
            aria-describedby="scan-help"
            spellCheck={false}
            placeholder="Scan the ticket QR code"
          />
          <button className="primary" type="submit" disabled={busy || code.trim() === ""}>
            {busy ? "Working…" : "Scan ticket"}
          </button>
        </form>

        {searchOpen ? (
          <div className="search-panel" ref={searchRef}>
            <div className="search-line">
              <label htmlFor="ticket-search">Find a ticket</label>
              <input
                id="ticket-search"
                ref={searchInputRef}
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Escape") {
                    event.preventDefault();
                    closeSearch();
                  }
                }}
                autoComplete="off"
                spellCheck={false}
                placeholder="Name, email address or phone number"
              />
              <div className="search-modes" role="group" aria-label="Search by">
                {MODES.map((mode) => (
                  <button
                    key={mode}
                    type="button"
                    aria-pressed={by === mode}
                    className={by === mode ? "is-current" : ""}
                    onClick={() => setBy(mode)}
                  >
                    {MODE_LABELS[mode]}
                  </button>
                ))}
              </div>
              <button type="button" onClick={closeSearch}>
                Back to QR scan
              </button>
            </div>
            {found && found.message && (
              <p className="hint" role="status">{found.message}</p>
            )}
            {found && found.rows.length > 0 && (
              <ul className="search-rows">
                {found.rows.map((row) => (
                  <li key={row.public_id}>
                    <button type="button" onClick={() => pick(row.public_id)}>
                      <strong>{row.name}</strong>
                      <span> · {row.ticket_type}</span>
                      {row.email_hint && <span> · {row.email_hint}</span>}
                      {row.phone_hint && <span> · {row.phone_hint}</span>}
                      {row.checked_in_message && <span> · {row.checked_in_message}</span>}
                    </button>
                  </li>
                ))}
              </ul>
            )}
            {found?.offline && (
              <p className="offline-note" role="status">
                Searching the tickets saved on this computer.
              </p>
            )}
          </div>
        ) : (
          <div className="search-launch">
            <button type="button" onClick={openSearch} disabled={busy}>
              Search by name, email or phone
            </button>
          </div>
        )}

        <section
          className={`result desk-${step}${view?.offline ? " is-offline" : ""}`}
          aria-live="polite"
        >
          <svg className="result-symbol" viewBox="0 0 48 48" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true"><path d="M16 7H7v9M32 7h9v9M7 32v9h9M41 32v9h-9"/><path d={step === "linked" ? "m14 24 7 7 14-14" : step === "error" || step === "needs_confirm" ? "M24 14v13m0 5v2" : "M16 19v10m6-13v16m5-16v16m5-13v10"}/></svg>
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
          {badge?.message && (
            <p className="badge-note" role="status">{badge.message}</p>
          )}
          <div className="actions">
            <button
              type="button"
              onClick={() => void run(() => deskLink(station.id, null))}
              disabled={busy || !view?.ticket || confirming}
            >
              Try the sticker again
            </button>
            {badge?.can_reprint && (
              <button
                type="button"
                onClick={() => view && print(view.session_id)}
                disabled={printing}
              >
                {printing ? "Printing…" : "Reprint badge"}
              </button>
            )}
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
        aria-labelledby="replace-title"
        onCancel={() => {
          setReason("");
          void run(() => deskReset(station.id));
        }}
      >
        <h2 id="replace-title">Replace this sticker?</h2>
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
            <button className="primary" type="submit" disabled={busy || reason.trim() === ""}>
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
