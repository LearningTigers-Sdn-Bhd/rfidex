import { useEffect, useState } from "react";

import { deskVerify, errorText } from "./api";
import type { StationStatus, VerifyView } from "./api";

/** The reader is asked again this long after each answer, never on a clock. */
const POLL_MS = 350;
/** A name stays up this long after the sticker leaves, so the guest can read it. */
const HOLD_MS = 4000;
const HOLD_PROBLEM_MS = 1200;

interface Shown {
  view: VerifyView;
  /** New for every tap, so the reveal plays again. */
  key: number;
}

/**
 * Full-screen sticker check. Rust decides what each answer means; this only
 * polls the reader while the screen is open and keeps a name on screen long
 * enough to read.
 */
export function Verify({ station }: { station: StationStatus }) {
  const [shown, setShown] = useState<Shown | null>(null);

  useEffect(() => {
    let live = true;
    let timer: number | undefined;
    let key = 0;
    let current: VerifyView | null = null;
    let lastSeen = 0;
    let gone = false;

    const apply = (next: VerifyView) => {
      const now = Date.now();
      if (next.state === "waiting") {
        if (!current) return;
        gone = true;
        const hold = current.state === "problem" ? HOLD_PROBLEM_MS : HOLD_MS;
        if (now - lastSeen > hold) {
          current = null;
          setShown(null);
        }
        return;
      }
      lastSeen = now;
      const same =
        current !== null &&
        current.state === next.state &&
        current.code === next.code &&
        current.sticker === next.sticker;
      if (same && !gone) return;
      gone = false;
      current = next;
      key += 1;
      setShown({ view: next, key });
    };

    const tick = async () => {
      try {
        apply(await deskVerify(station.id));
      } catch (problem) {
        apply({ state: "problem", code: "app_error", message: errorText(problem), holder: null, sticker: null });
      }
      if (live) timer = window.setTimeout(tick, POLL_MS);
    };
    void tick();
    return () => {
      live = false;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [station.id]);

  const state = shown?.view.state ?? "waiting";
  return (
    <section className={`verify verify-${state}`} aria-live="polite">
      {shown ? <Reveal key={shown.key} view={shown.view} /> : <Idle offline={!station.online} />}
      <p className="verify-foot">Checking only. Nothing is linked, printed or checked in.</p>
    </section>
  );
}

function Idle({ offline }: { offline: boolean }) {
  return (
    <div className="verify-card verify-idle">
      <div className="verify-pulse" aria-hidden="true">
        <svg viewBox="0 0 240 240" fill="none">
          <circle className="ring r1" cx="120" cy="120" r="44" />
          <circle className="ring r2" cx="120" cy="120" r="44" />
          <circle className="ring r3" cx="120" cy="120" r="44" />
          <circle className="core" cx="120" cy="120" r="44" />
          <g className="waves" strokeLinecap="round">
            <circle cx="108" cy="120" r="4.5" className="dot" />
            <path d="M121 107a18 18 0 0 1 0 26" />
            <path d="M131 98a31 31 0 0 1 0 44" />
            <path d="M141 90a44 44 0 0 1 0 60" />
          </g>
        </svg>
      </div>
      <h1>Hold a sticker near the reader</h1>
      <p className="verify-sub">The guest's name appears here.</p>
      {offline && <p className="verify-offline">Offline. Verifying needs a connection to the server.</p>}
    </div>
  );
}

function Reveal({ view }: { view: VerifyView }) {
  const holder = view.holder;
  return (
    <div className="verify-card verify-reveal">
      <Disc state={view.state} />
      {holder ? (
        <>
          <h1 className="verify-name">{holder.name}</h1>
          <p className="verify-meta">
            <span>{holder.ticket_type}</span>
            <span className={holder.checked_in ? "pill in" : "pill"}>{holder.checked_in ? "Checked in" : "Not checked in yet"}</span>
          </p>
          <p className="verify-sub">{view.message}</p>
        </>
      ) : (
        <h1 className="verify-name plain">{view.message}</h1>
      )}
      {view.sticker && <p className="verify-sticker">Sticker {view.sticker}</p>}
    </div>
  );
}

function Disc({ state }: { state: VerifyView["state"] }) {
  return (
    <svg className="verify-disc" viewBox="0 0 120 120" aria-hidden="true" fill="none">
      <circle className="disc-bg" cx="60" cy="60" r="56" />
      {state === "verified" && <path className="mark draw" d="M36 62l16 16 32-34" />}
      {state === "invalid" && <path className="mark draw" d="M42 42l36 36M78 42L42 78" />}
      {state === "unknown" && <path className="mark" d="M46 47a14 14 0 1 1 20 12c-4 2-6 5-6 10M60 84v.5" />}
      {state === "problem" && <path className="mark" d="M60 34v32M60 84v.5" />}
    </svg>
  );
}
