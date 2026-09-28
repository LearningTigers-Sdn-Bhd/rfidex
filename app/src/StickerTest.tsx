// The disposable-sticker write test for one saved desk.
//
// Rust runs every step and writes every result sentence; this file only holds
// the DISPOSABLE confirmation, the busy state and the two-step turn-on.
import { useEffect, useState } from "react";
import { errorText, stickerTest, stickerWriting } from "./api";
import type { AppView, StickerTestStep, StickerTestView } from "./api";

interface Props {
  stationId: string;
  onSaved: (view: AppView) => void;
}

export default function StickerTest({ stationId, onSaved }: Props) {
  const [view, setView] = useState<StickerTestView | null>(null);
  const [disposable, setDisposable] = useState(false);
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    stickerTest(stationId, "status")
      .then((v) => live && setView(v))
      .catch((e) => live && setFailure(errorText(e)));
    return () => {
      live = false;
    };
  }, [stationId]);

  const run = async (step: StickerTestStep) => {
    if (busy) return;
    setBusy(true);
    setFailure(null);
    try {
      setView(await stickerTest(stationId, step));
    } catch (e) {
      setFailure(errorText(e));
    } finally {
      // Every new sticker needs its own confirmation.
      if (step !== "tear_write") setDisposable(false);
      setBusy(false);
    }
  };

  const setWriting = async (on: boolean) => {
    setBusy(true);
    setFailure(null);
    try {
      onSaved(await stickerWriting(stationId, on));
    } catch (e) {
      setFailure(errorText(e));
      setBusy(false);
    }
    setConfirming(false);
  };

  const tally = view?.tally;
  const tearsCaught = tally ? tally.tears - tally.tears_undetected : 0;

  return (
    <div className="hardware-field sticker-test">
      <p className="eyebrow">Sticker write test</p>
      <p className="field-help">
        Proves this reader writes stickers safely before the desk is allowed to.
        Use only stickers marked DISPOSABLE: the test writes to them and a tear
        test can spoil them.
      </p>
      {tally && (
        <p className="field-help" role="status">
          Stickers passed: {tally.passed} of {tally.target_stickers} · Tears caught:{" "}
          {tearsCaught} of {tally.target_tears} · Failures:{" "}
          {tally.failed + tally.tears_undetected} · Writing is{" "}
          <strong>{view?.write_enabled ? "on" : "off"}</strong>
        </p>
      )}

      <label className="checkbox">
        <input
          type="checkbox"
          checked={disposable}
          onChange={(e) => setDisposable(e.target.checked)}
          disabled={busy}
        />
        The sticker on the reader is marked DISPOSABLE
      </label>

      <div className="actions">
        <button type="button" onClick={() => void run("check")} disabled={busy || !disposable}>
          {busy ? "Testing…" : "Test this sticker"}
        </button>
        <button type="button" onClick={() => void run("tear_write")} disabled={busy || !disposable}>
          Start tear
        </button>
        <button
          type="button"
          onClick={() => void run("tear_check")}
          disabled={busy || !view?.tear_waiting}
        >
          Check tear
        </button>
      </div>
      <p className="field-help">
        Tear test: hold the sticker at the edge of the reader, press Start tear
        and pull the sticker away at once. Then put it back and press Check tear.
      </p>

      {view?.message && (
        <div className={view.ok ? "note" : "failure"} role="status">
          <p>{view.message}</p>
          {view.details.length > 0 && (
            <ul>
              {view.details.map((line) => (
                <li key={line}>{line}</li>
              ))}
            </ul>
          )}
        </div>
      )}
      {failure && (
        <p className="failure" role="alert">
          {failure}
        </p>
      )}

      <div className="actions">
        {view?.write_enabled ? (
          <button type="button" onClick={() => void setWriting(false)} disabled={busy}>
            Turn writing off
          </button>
        ) : confirming && tally ? (
          <div className="failure" role="alertdialog">
            <p>
              {tally.passed} of {tally.target_stickers} stickers and {tearsCaught} of{" "}
              {tally.target_tears} tears have passed so far; the full test asks for
              more. Turn writing on for this desk anyway? The stations restart.
            </p>
            <div className="actions">
              <button type="button" onClick={() => void setWriting(true)} disabled={busy}>
                Turn writing on
              </button>
              <button type="button" onClick={() => setConfirming(false)} disabled={busy}>
                Cancel
              </button>
            </div>
          </div>
        ) : (
          <button
            type="button"
            onClick={() => setConfirming(true)}
            disabled={busy || !view?.can_enable}
          >
            Turn writing on
          </button>
        )}
      </div>
    </div>
  );
}
