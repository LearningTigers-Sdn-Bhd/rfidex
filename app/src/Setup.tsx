import { useEffect, useRef, useState } from "react";
import type { FormEvent } from "react";

import { errorText, setupGet, setupSave, setupTest } from "./api";
import type {
  AppStatus,
  AppView,
  ConnectionView,
  GateKind,
  Role,
  StationConfig,
  StationKind,
} from "./api";

interface Props {
  hasSavedConfig: boolean;
  status: AppStatus | null;
  onSaved: (view: AppView) => void;
  onCancel: () => void;
}

interface RoleChange {
  name: string;
  from: Role;
  to: Role;
}

/**
 * Setup opens freely: there is no PIN and no staff login, by design. The one
 * secret is the API key, which is held in this component's memory only and is
 * never written to storage, a URL, or a log.
 */
export function Setup({ hasSavedConfig, status, onSaved, onCancel }: Props) {
  const [serverUrl, setServerUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [stations, setStations] = useState<StationConfig[]>([]);
  const [original, setOriginal] = useState<StationConfig[]>([]);
  const [keyOnFile, setKeyOnFile] = useState(hasSavedConfig);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const [tested, setTested] = useState<ConnectionView | null>(null);
  const [roleChange, setRoleChange] = useState<RoleChange[] | null>(null);
  const dialogRef = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    let live = true;
    setupGet()
      .then((view) => {
        if (!live || !view) return;
        setServerUrl(view.server_url);
        setStations(view.stations);
        setOriginal(view.stations);
        setKeyOnFile(view.has_api_key);
      })
      .catch((problem) => live && setFailure(errorText(problem)));
    return () => {
      live = false;
    };
  }, []);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (roleChange && !dialog.open) dialog.showModal();
    else if (!roleChange && dialog.open) dialog.close();
  }, [roleChange]);

  const patch = (id: string, update: Partial<StationConfig>) =>
    setStations((list) => list.map((s) => (s.id === id ? { ...s, ...update } : s)));

  const setGate = (
    id: string,
    update: Partial<{ gate_kind: GateKind; release_verified: boolean }>,
  ) =>
    setStations((list) =>
      list.map((s) =>
        s.id === id ? { ...s, device: { ...asSimGate(s), ...update } } : s,
      ),
    );

  const changeKind = (id: string, kind: StationKind) =>
    setStations((list) =>
      list.map((s) =>
        s.id === id
          ? {
              ...s,
              kind,
              // A desk has no direction, and a gate must have one.
              role: kind === "gate" ? s.role ?? "entry" : null,
              device:
                kind === "gate"
                  ? asSimGate(s)
                  : s.device.type === "sim_desk"
                    ? s.device
                    : { type: "sim_desk" as const },
            }
          : s,
      ),
    );

  const addStation = () =>
    setStations((list) => [
      ...list,
      {
        id: crypto.randomUUID(),
        name: `Station ${list.length + 1}`,
        kind: "desk",
        role: null,
        device: { type: "sim_desk" },
        debounce_secs: 5,
        write_start_block: 0,
      },
    ]);

  const testConnection = async () => {
    setBusy(true);
    try {
      setTested(await setupTest(serverUrl.trim(), apiKey));
      setFailure(null);
    } catch (problem) {
      setFailure(errorText(problem));
      setTested(null);
    } finally {
      setBusy(false);
    }
  };

  const reallySave = async () => {
    setBusy(true);
    try {
      const view = await setupSave({
        server_url: serverUrl.trim(),
        api_key: apiKey,
        stations,
      });
      // The secret leaves the form as soon as the runtime has it.
      setApiKey("");
      setTested(null);
      setFailure(null);
      onSaved(view);
    } catch (problem) {
      // Keep every edit on screen so the operator can fix it.
      setFailure(errorText(problem));
    } finally {
      setBusy(false);
      setRoleChange(null);
    }
  };

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (busy) return;
    const changes = roleChanges(original, stations);
    if (changes.length > 0) {
      setRoleChange(changes);
      return;
    }
    void reallySave();
  };

  const mode = status?.stations.find((station) => station.mode)?.mode ?? null;

  return (
    <div className="panel setup">
      <div className="panel-head">
        <h1>Setup</h1>
        {hasSavedConfig && (
          <button type="button" onClick={onCancel} disabled={busy}>
            Close without saving
          </button>
        )}
      </div>

      <p className="hint">
        Anyone at this computer can open Setup. The saved API key is never shown
        again here, and it is only used to talk to the event server.
      </p>

      <form onSubmit={submit}>
        <fieldset>
          <legend>Event server</legend>
          <label htmlFor="server-url">Server address</label>
          <input
            id="server-url"
            value={serverUrl}
            onChange={(event) => setServerUrl(event.target.value)}
            autoComplete="off"
            spellCheck={false}
            placeholder="https://events.example.com"
          />

          <label htmlFor="api-key">API key</label>
          <input
            id="api-key"
            type="password"
            value={apiKey}
            onChange={(event) => setApiKey(event.target.value)}
            autoComplete="new-password"
            placeholder={keyOnFile ? "Leave blank to keep current key" : "Paste the event API key"}
          />

          <div className="actions">
            <button type="button" onClick={() => void testConnection()} disabled={busy}>
              Test connection
            </button>
          </div>
          {tested && (
            <p className={tested.ok ? "note" : "failure"} role="status">
              {tested.message}
            </p>
          )}

          <p className="hint">
            Event mode: <strong>{modeName(mode)}</strong> — set by the server, not
            here.
          </p>
        </fieldset>

        <fieldset>
          <legend>Stations on this computer</legend>
          {stations.length === 0 && (
            <p className="empty">No stations yet. Add the ones this PC runs.</p>
          )}
          <ul className="station-editor">
            {stations.map((station) => (
              <li key={station.id}>
                <div className="row">
                  <label>
                    Name
                    <input
                      value={station.name}
                      onChange={(event) => patch(station.id, { name: event.target.value })}
                      autoComplete="off"
                    />
                  </label>
                  <label>
                    Type
                    <select
                      value={station.kind}
                      onChange={(event) =>
                        changeKind(station.id, event.target.value as StationKind)
                      }
                    >
                      <option value="desk">Desk</option>
                      <option value="gate">Gate</option>
                    </select>
                  </label>
                  {station.kind === "gate" && (
                    <label>
                      Direction
                      <select
                        value={station.role ?? "entry"}
                        onChange={(event) =>
                          patch(station.id, { role: event.target.value as Role })
                        }
                      >
                        <option value="entry">Entry</option>
                        <option value="exit">Exit</option>
                      </select>
                    </label>
                  )}
                  <label>
                    Wait between repeats (seconds)
                    <input
                      type="number"
                      min={1}
                      max={60}
                      value={station.debounce_secs}
                      onChange={(event) =>
                        patch(station.id, { debounce_secs: Number(event.target.value) })
                      }
                    />
                  </label>
                  <label>
                    Write start block
                    <input
                      type="number"
                      min={0}
                      max={255}
                      value={station.write_start_block}
                      onChange={(event) =>
                        patch(station.id, { write_start_block: Number(event.target.value) })
                      }
                    />
                  </label>
                </div>

                {station.device.type === "sim_gate" && (
                  <div className="row">
                    <label>
                      Simulated gate output
                      <select
                        value={station.device.gate_kind}
                        onChange={(event) =>
                          setGate(station.id, {
                            gate_kind: event.target.value as GateKind,
                          })
                        }
                      >
                        <option value="records">Stored records</option>
                        <option value="live_inventory">Live inventory</option>
                      </select>
                    </label>
                    <label className="checkbox">
                      <input
                        type="checkbox"
                        checked={station.device.release_verified}
                        onChange={(event) =>
                          setGate(station.id, { release_verified: event.target.checked })
                        }
                      />
                      Simulator only: the reader acknowledges releases. This says
                      nothing about real hardware.
                    </label>
                  </div>
                )}

                <div className="actions">
                  <button type="button" onClick={() => setStations((list) => list.filter((s) => s.id !== station.id))}>
                    Remove station
                  </button>
                </div>
              </li>
            ))}
          </ul>
          <div className="actions">
            <button type="button" onClick={addStation}>
              Add station
            </button>
            <button type="submit" disabled={busy}>
              {busy ? "Saving…" : "Save setup"}
            </button>
          </div>
        </fieldset>

        {failure && (
          <p className="failure" role="alert">
            {failure}
          </p>
        )}
      </form>

      <dialog ref={dialogRef} className="confirm" onCancel={() => setRoleChange(null)}>
        <h2>Change a gate direction?</h2>
        <p>
          The direction decides which way passages are recorded for that gate.
          Everybody using this computer will see the new direction.
        </p>
        <ul>
          {roleChange?.map((change) => (
            <li key={change.name}>
              <strong>{change.name}</strong>: {change.from} → {change.to}
            </li>
          ))}
        </ul>
        <div className="dialog-actions">
          <button type="button" onClick={() => setRoleChange(null)}>
            Cancel — keep the old direction
          </button>
          <button type="button" onClick={() => void reallySave()} disabled={busy}>
            Change it and save
          </button>
        </div>
      </dialog>
    </div>
  );
}

/** The gate settings for a station, defaulting when it is not a gate yet. */
function asSimGate(station: StationConfig) {
  return station.device.type === "sim_gate"
    ? station.device
    : { type: "sim_gate" as const, gate_kind: "records" as const, release_verified: false };
}

/** Directions that changed on a gate that already existed. */
function roleChanges(before: StationConfig[], after: StationConfig[]): RoleChange[] {
  const changes: RoleChange[] = [];
  for (const next of after) {
    const was = before.find((station) => station.id === next.id);
    if (!was || was.kind !== "gate" || next.kind !== "gate") continue;
    if (was.role && next.role && was.role !== next.role) {
      changes.push({ name: next.name, from: was.role, to: next.role });
    }
  }
  return changes;
}

function modeName(mode: "bind" | "write" | null): string {
  if (mode === "write") return "Write";
  if (mode === "bind") return "Bind";
  return "not known yet";
}
