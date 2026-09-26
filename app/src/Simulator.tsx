import { useRef, useState } from "react";
import type { FormEvent } from "react";

import { errorText, simClear, simPass, simPlace, simSetConnected } from "./api";
import type { StationStatus } from "./api";

interface Props {
  station: StationStatus;
}

const FIRST_STICKER = "3412CDAB500104E0";

/**
 * The simulator panel stands in for hardware that is not here yet. It never
 * hides a real action: it is visually separated from the operator screens and
 * only appears for a simulated station.
 */
export function Simulator({ station }: Props) {
  const [uid, setUid] = useState(FIRST_STICKER);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const dialogRef = useRef<HTMLDialogElement>(null);

  if (!station.simulated) return null;

  const run = async (operation: () => Promise<void>, done: string) => {
    setBusy(true);
    try {
      await operation();
      setFailure(null);
      setNote(done);
    } catch (problem) {
      setFailure(errorText(problem));
      setNote(null);
    } finally {
      setBusy(false);
    }
  };

  const place = (event: FormEvent) => {
    event.preventDefault();
    void run(() => simPlace(station.id, uid.trim()), "Sticker placed on the reader.");
  };

  const isDesk = station.kind === "desk";

  return (
    <aside className="simulator" aria-label="Simulator">
      <div><strong>Hardware simulator</strong><span className="hint">Test stickers and reader connection</span></div>
      <button type="button" onClick={() => dialogRef.current?.showModal()}>Open simulator</button>
      <dialog ref={dialogRef} className="confirm simulator-dialog" aria-labelledby="simulator-title">
      <div className="panel-head"><h2 id="simulator-title">Simulator</h2><button type="button" onClick={() => dialogRef.current?.close()}>Close simulator</button></div>
      <p className="hint">
        This station is simulated. Nothing here is real hardware.
      </p>

      <form onSubmit={place}>
        <label htmlFor="sim-uid">Sticker UID (hex)</label>
        <input
          id="sim-uid"
          value={uid}
          onChange={(event) => setUid(event.target.value)}
          autoComplete="off"
          spellCheck={false}
        />
        <div className="actions">
          <button type="submit" disabled={busy || uid.trim() === ""}>
            {isDesk ? "Place sticker" : "Walk through"}
          </button>
          {isDesk && (
            <button
              type="button"
              onClick={() => void run(() => simClear(station.id), "Reader cleared.")}
              disabled={busy}
            >
              Remove stickers
            </button>
          )}
        </div>
      </form>

      <p className="hint">
        {isDesk
          ? "Place two different UIDs to test the one-sticker rule. Remove a written sticker before walking it past a gate."
          : "A sticker on a desk reader cannot walk through a gate until it is removed there."}
      </p>

      <label className="checkbox">
        <input
          type="checkbox"
          checked={station.connected}
          disabled={busy}
          onChange={(event) =>
            void run(
              () => simSetConnected(station.id, event.target.checked),
              event.target.checked ? "Reader connected." : "Reader disconnected.",
            )
          }
        />
        Reader connected
      </label>

      {note && <p className="note">{note}</p>}
      {failure && (
        <p className="failure" role="alert">
          {failure}
        </p>
      )}
      </dialog>
    </aside>
  );
}
