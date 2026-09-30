import { useEffect, useRef, useState } from "react";

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
  const stage = useRef<HTMLElement>(null);
  const [full, setFull] = useState(false);

  useEffect(() => {
    const sync = () => setFull(document.fullscreenElement === stage.current);
    document.addEventListener("fullscreenchange", sync);
    return () => {
      document.removeEventListener("fullscreenchange", sync);
      if (document.fullscreenElement === stage.current) void document.exitFullscreen();
    };
  }, []);

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
    <section ref={stage} className={`verify verify-${state}`} aria-live="polite">
      {!full && (
        <button
          type="button"
          className="verify-full"
          aria-label="Full screen"
          title="Full screen"
          onClick={() => void stage.current?.requestFullscreen().catch(() => {})}
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" />
          </svg>
        </button>
      )}
      {shown ? <Reveal key={shown.key} view={shown.view} /> : <Idle offline={!station.online} />}
    </section>
  );
}

function Idle({ offline }: { offline: boolean }) {
  return (
    <div className="verify-card verify-idle">
      <p className="verify-label">Check your tag</p>
      <div className="verify-pulse" aria-hidden="true">
        <span className="ripple" /><span className="ripple" /><span className="ripple" />
        <svg viewBox="0 0 240 240" fill="none">
          <circle className="ring" cx="120" cy="120" r="76" />
          <circle className="core" cx="120" cy="120" r="56" />
          <g className="waves" strokeLinecap="round" strokeLinejoin="round">
            <rect x="90" y="96" width="32" height="48" rx="6" />
            <path d="M101 107h10M133 109q11 11 0 22M142 101q19 19 0 38" />
          </g>
        </svg>
      </div>
      <h1>Hold your tag<br />near the reader</h1>
      {offline && <p className="verify-offline">Offline. Verifying needs a connection to the server.</p>}
    </div>
  );
}

function Reveal({ view }: { view: VerifyView }) {
  const holder = view.holder;
  return (
    <div className="verify-card verify-reveal">
      <p className="verify-label">Tag verification</p>
      <Disc state={view.state} />
      {holder ? (
        <>
          <h1 className="verify-name">{holder.name}</h1>
          <p className="verify-sub">{view.message}</p>
        </>
      ) : (
        <h1 className="verify-name plain">{view.message}</h1>
      )}
    </div>
  );
}

/** Directions the sparkles fly off in, evenly round the disc. */
const SPARKS = [0, 45, 90, 135, 180, 225, 270, 315];

function Disc({ state }: { state: VerifyView["state"] }) {
  return (
    <svg className="verify-disc" viewBox="0 0 120 120" aria-hidden="true" fill="none">
      {state === "verified" && (
        <>
          <circle className="burst" cx="60" cy="60" r="56" />
          <circle className="burst late" cx="60" cy="60" r="56" />
          {SPARKS.map((angle) => (
            <circle key={angle} className="spark" cx="60" cy="60" r="2.6" style={{ "--a": `${angle}deg` } as React.CSSProperties} />
          ))}
        </>
      )}
      <circle className="disc-bg" cx="60" cy="60" r="56" />
      {state === "verified" && <path className="mark draw" d="M36 62l16 16 32-34" />}
      {state === "invalid" && (
        <g className="shake">
          <path className="mark draw" d="M42 42l36 36M78 42L42 78" />
        </g>
      )}
      {state === "unknown" && (
        <g className="wonder">
          <path className="mark" d="M47 48c0-9 6-15 13-15s14 5 14 13c0 7-5 10-10 14-3 2-4 5-4 9M60 87v.5" />
        </g>
      )}
      {state === "problem" && (
        <g className="bounce">
          <path className="mark" d="M60 34v32M60 84v.5" />
        </g>
      )}
    </svg>
  );
}
