import { useEffect, useRef, useState } from "react";
import type { FormEvent } from "react";

import { errorText, setupGet, setupSave, setupTest, setupTestPrinter, updateCheck, updateInstall } from "./api";
import type {
  AppStatus,
  AppView,
  ConnectionView,
  DeviceChoice,
  GateKind,
  HardwareConfig,
  RealDevice,
  Role,
  StationConfig,
  StationKind,
  UpdateView,
} from "./api";
import HardwareFields, { defaultHardware, fitHardware } from "./HardwareFields";

const DEFAULT_PRINTER_URL = "http://127.0.0.1:8000";

/** Simulated readers exist only in development builds; staff get real ones. */
const SIMULATOR = import.meta.env.DEV;

const realDevice = (kind: StationKind, hardware: HardwareConfig): RealDevice =>
  ({ type: kind === "desk" ? "ecrfid_desk" : "ecrfid_gate", hardware }) as RealDevice;

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
  // Keyed by station id, and only shown while the address it tested is still
  // the address on screen: a changed or removed station's result is ignored.
  const [printerTests, setPrinterTests] = useState<
    Record<string, { url: string; view: ConnectionView }>
  >({});
  const [roleChange, setRoleChange] = useState<RoleChange[] | null>(null);
  const [section, setSection] = useState<"server" | "stations" | "about">("server");
  const dialogRef = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    let live = true;
    setupGet()
      .then((view) => {
        if (!live || !view) return;
        setServerUrl(view.server_url);
        // A release build has no simulator: a station saved with one needs a
        // real reader before it can start again. A real reader is fitted to
        // its kind, so only the fields that kind uses are shown.
        setStations(
          view.stations.map((s) => {
            if (s.device.type === "ecrfid_desk" || s.device.type === "ecrfid_gate") {
              return { ...s, device: realDevice(s.kind, fitHardware(s.kind, s.device.hardware)) };
            }
            return SIMULATOR ? s : { ...s, device: realDevice(s.kind, defaultHardware(s.kind)) };
          }),
        );
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
              device: asKindDevice(s, kind),
              // A gate may have no printer address; a desk needs one.
              printer_url:
                kind === "desk" && s.printer_url.trim() === ""
                  ? DEFAULT_PRINTER_URL
                  : s.printer_url,
            }
          : s,
      ),
    );

  /** Keep a real reader when the kind still fits it, and drop it when it does
   * not: a real desk reader is not a gate reader. */
  const setRealReader = (id: string, hardware: HardwareConfig) =>
    setStations((list) =>
      list.map((s) => {
        if (s.id !== id) return s;
        const type = s.kind === "desk" ? "ecrfid_desk" : "ecrfid_gate";
        return { ...s, device: { type, hardware } as RealDevice };
      }),
    );

  /** Pick a simulated or a real reader for this station. */
  const setReaderType = (id: string, choice: "sim" | "real") =>
    setStations((list) =>
      list.map((s) => {
        if (s.id !== id) return s;
        if (choice === "real") {
          const hardware =
            s.device.type === "ecrfid_desk" || s.device.type === "ecrfid_gate"
              ? fitHardware(s.kind, s.device.hardware)
              : defaultHardware(s.kind);
          return {
            ...s,
            device: {
              type: s.kind === "desk" ? "ecrfid_desk" : "ecrfid_gate",
              hardware,
            } as RealDevice,
          };
        }
        return { ...s, device: asKindDevice(s, s.kind, true) };
      }),
    );

  const addStation = () =>
    setStations((list) => [
      ...list,
      {
        id: crypto.randomUUID(),
        name: `Station ${list.length + 1}`,
        kind: "desk",
        role: null,
        device: SIMULATOR ? { type: "sim_desk" } : realDevice("desk", defaultHardware("desk")),
        debounce_secs: 5,
        write_start_block: 0,
        printer_url: DEFAULT_PRINTER_URL,
        alarm_all_panels: true,
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

  const testPrinter = async (station: StationConfig) => {
    const url = station.printer_url.trim();
    setBusy(true);
    try {
      const view = await setupTestPrinter(url);
      setPrinterTests((all) => ({ ...all, [station.id]: { url, view } }));
      setFailure(null);
    } catch (problem) {
      setFailure(errorText(problem));
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
  // A reader test targets the station the app has running, so an unsaved edit
  // means the thing on screen is not the thing that would be tested.
  const isDirty = JSON.stringify(stations) !== JSON.stringify(original);
  // A hardware test runs against the saved config for a station id, so only a
  // station that is new or edited must block its own test buttons.
  const stationDirty = (station: StationConfig) => {
    const saved = original.find((o) => o.id === station.id);
    return !saved || JSON.stringify(saved) !== JSON.stringify(station);
  };

  return (
    <div className="subpage setup-page">
      <nav className="subnav" aria-label="Setup sections">
        <p className="nav-lbl">Setup</p>
        <button
          type="button"
          className={section === "server" ? "subnav-btn is-active" : "subnav-btn"}
          aria-current={section === "server" ? "page" : undefined}
          onClick={() => setSection("server")}
        >
          Event server
        </button>
        <button
          type="button"
          className={section === "stations" ? "subnav-btn is-active" : "subnav-btn"}
          aria-current={section === "stations" ? "page" : undefined}
          onClick={() => setSection("stations")}
        >
          Stations
        </button>
        <button
          type="button"
          className={section === "about" ? "subnav-btn is-active" : "subnav-btn"}
          aria-current={section === "about" ? "page" : undefined}
          onClick={() => setSection("about")}
        >
          About &amp; updates
        </button>
      </nav>

      <div className="subpage-content setup-content">
        <header className="setup-head">
          <div>
            <p className="eyebrow">RfiDex / Configuration</p>
            <h1>Setup</h1>
          </div>
        </header>

        <p className="hint">
          Point this computer at its EventzFlow server and declare the readers
          plugged into it. The saved API key is never shown again — it only
          authenticates this machine to the server.
        </p>

        <div className="setup-body">
        <form id="setup-form" onSubmit={submit} className="setup-form">
        <section className={section === "server" ? "setup-section is-active" : "setup-section"}>
          <h2 className="setup-section-title">Event server</h2>
          <p className="setup-section-sub">The deployment this computer reports to, and the key that links it to one event.</p>
          <div className="row two">
            <div>
              <label htmlFor="server-url">Server URL</label>
              <input
                id="server-url"
                value={serverUrl}
                onChange={(event) => setServerUrl(event.target.value)}
                autoComplete="off"
                spellCheck={false}
                placeholder="https://events.example.com"
              />
              <small className="field-help">
                The EventzFlow deployment this station reports to, e.g.
                https://events.example.com.
              </small>
            </div>
            <div>
              <label htmlFor="api-key">API key</label>
              <input
                id="api-key"
                type="password"
                value={apiKey}
                onChange={(event) => setApiKey(event.target.value)}
                autoComplete="new-password"
                placeholder={keyOnFile ? "Leave blank to keep current key" : "Paste the event API key"}
              />
              <small className="field-help">
                In EventzFlow, open the event's API keys and copy the RFID one.
                It links this computer to that event.
              </small>
            </div>
          </div>

          <div className="actions end">
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
            Event mode: <strong>{modeName(mode)}</strong> — read from the server
            on connect; not editable here.
          </p>
        </section>

        <section className={section === "stations" ? "setup-section is-active" : "setup-section"}>
          <h2 className="setup-section-title">Stations on this computer</h2>
          <p className="setup-section-sub">One station per RFID reader plugged into this computer — a desk that links tickets, or a gate that records guests passing.</p>
          {stations.length === 0 && (
            <p className="empty">No stations yet. Add the ones this PC runs.</p>
          )}
          <ul className="station-editor">
            {stations.map((station) => {
              const kindLabel = station.kind === "desk" ? "Desk" : station.role === "exit" ? "Exit gate" : "Entry gate";
              const isReal = station.device.type === "ecrfid_desk" || station.device.type === "ecrfid_gate";
              return (
              <li key={station.id} className={`station-card is-${station.kind}`}>
                <header className="station-card-head">
                  <span className="station-card-icon" aria-hidden="true">
                    {station.kind === "desk" ? (
                      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round"><rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8M12 16v4"/></svg>
                    ) : (
                      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round"><path d="M4 21V8l8-5 8 5v13"/><path d="M9 21v-6h6v6"/></svg>
                    )}
                  </span>
                  <input
                    className="station-card-name"
                    aria-label="Station name"
                    value={station.name}
                    onChange={(event) => patch(station.id, { name: event.target.value })}
                    autoComplete="off"
                    placeholder="Station name"
                  />
                  <span className="station-card-chip">{kindLabel}</span>
                  {isReal && <span className="station-card-real">Real reader</span>}
                  <button
                    type="button"
                    className="station-card-remove"
                    title="Remove station"
                    aria-label={`Remove ${station.name}`}
                    onClick={() => setStations((list) => list.filter((s) => s.id !== station.id))}
                  >
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round"><path d="M3 6h18M8 6V4a1 1 0 0 1 1-1h6a1 1 0 0 1 1 1v2m2 0v14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2V6h12z"/></svg>
                  </button>
                </header>

                <div className="station-card-body">
                <div className="station-fields">
                  <div className="field">
                    <label htmlFor={`kind-${station.id}`}>Type</label>
                    <select
                      id={`kind-${station.id}`}
                      value={station.kind}
                      onChange={(event) =>
                        changeKind(station.id, event.target.value as StationKind)
                      }
                    >
                      <option value="desk">Desk</option>
                      <option value="gate">Gate</option>
                    </select>
                  </div>
                  {SIMULATOR && (
                    <div className="field">
                      <label htmlFor={`reader-${station.id}`}>Reader</label>
                      <select
                        id={`reader-${station.id}`}
                        value={isReal ? "real" : "sim"}
                        onChange={(event) =>
                          setReaderType(station.id, event.target.value as "sim" | "real")
                        }
                      >
                        <option value="sim">Simulator</option>
                        <option value="real">Real reader</option>
                      </select>
                    </div>
                  )}
                </div>

                {(station.kind === "gate" || station.kind === "desk") && (
                  <div className="station-cluster">
                    {station.kind === "gate" && (
                      <div className="field">
                        <label htmlFor={`role-${station.id}`}>Direction</label>
                        <select
                          id={`role-${station.id}`}
                          value={station.role ?? "entry"}
                          onChange={(event) =>
                            patch(station.id, { role: event.target.value as Role })
                          }
                        >
                          <option value="entry">Entry</option>
                          <option value="exit">Exit</option>
                        </select>
                        <small className="field-help">
                          Counts guests coming in (entry) or leaving (exit).
                        </small>
                      </div>
                    )}
                    {station.kind === "gate" && (
                      <div className="field">
                        <label htmlFor={`debounce-${station.id}`}>Repeat window (s)</label>
                        <input
                          id={`debounce-${station.id}`}
                          type="number"
                          min={1}
                          max={60}
                          value={station.debounce_secs}
                          onChange={(event) =>
                            patch(station.id, { debounce_secs: Number(event.target.value) })
                          }
                        />
                        <small className="field-help">
                          A tag held near the reader counts once per window. 5 suits most gates.
                        </small>
                      </div>
                    )}
                    {station.kind === "gate" && station.device.type === "ecrfid_gate" && (
                      <div className="field">
                        <label htmlFor={`alarm-${station.id}`}>Alarm</label>
                        <select
                          id={`alarm-${station.id}`}
                          value={station.alarm_all_panels ? "all" : "one"}
                          onChange={(event) =>
                            patch(station.id, { alarm_all_panels: event.target.value === "all" })
                          }
                        >
                          <option value="all">All panels sound</option>
                          <option value="one">Only the panel that read the sticker</option>
                        </select>
                        <small className="field-help">
                          What a declined pass sets off on this gate.
                        </small>
                      </div>
                    )}
                    {station.kind === "desk" && mode === "write" && (
                      <div className="field">
                        <label htmlFor={`block-${station.id}`}>Write start block</label>
                        <input
                          id={`block-${station.id}`}
                          type="number"
                          min={0}
                          max={255}
                          value={station.write_start_block}
                          onChange={(event) =>
                            patch(station.id, { write_start_block: Number(event.target.value) })
                          }
                        />
                        <small className="field-help">
                          Where on the sticker the ticket is written. Leave at 0 unless the RFID supplier says otherwise.
                        </small>
                      </div>
                    )}
                    {station.kind === "desk" && (
                      <div className="printer-field">
                        <label htmlFor={`printer-${station.id}`}>Printer address</label>
                        <div className="inline-test">
                          <input
                            id={`printer-${station.id}`}
                            value={station.printer_url}
                            onChange={(event) =>
                              patch(station.id, { printer_url: event.target.value })
                            }
                            autoComplete="off"
                            spellCheck={false}
                            placeholder={DEFAULT_PRINTER_URL}
                          />
                          <button
                            type="button"
                            onClick={() => void testPrinter(station)}
                            disabled={busy}
                          >
                            Test printer
                          </button>
                        </div>
                        <small className="field-help">
                          Local badge-printer bridge, not the EventzFlow server.
                          Default http://127.0.0.1:8000 — start the bridge, then test.
                        </small>
                        {printerTests[station.id]?.url === station.printer_url.trim() && (
                          <p
                            className={printerTests[station.id].view.ok ? "note" : "failure"}
                            role="status"
                          >
                            {printerTests[station.id].view.message}
                          </p>
                        )}
                      </div>
                    )}
                  </div>
                )}

                {(station.device.type === "ecrfid_desk" ||
                station.device.type === "ecrfid_gate") && (
                <HardwareFields
                  station={station}
                  dirty={stationDirty(station)}
                  onChange={(hardware) => setRealReader(station.id, hardware)}
                  onSaved={onSaved}
                />
              )}

              {SIMULATOR && station.device.type === "sim_gate" && (
                  <div className="station-cluster">
                    <div className="field">
                      <label htmlFor={`gatekind-${station.id}`}>Simulated gate output</label>
                      <select
                        id={`gatekind-${station.id}`}
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
                      <small className="field-help">
                        Stored records keeps a list of passes; live inventory reports every tag in range.
                      </small>
                    </div>
                    <div className="sim-box">
                      <label className="checkbox">
                        <input
                          type="checkbox"
                          checked={station.device.release_verified}
                          onChange={(event) =>
                            setGate(station.id, { release_verified: event.target.checked })
                          }
                        />
                        <span>Simulator acknowledges releases (says nothing about real hardware)</span>
                      </label>
                    </div>
                  </div>
                )}

                </div>
              </li>
              );
            })}
          </ul>
          <div className="actions">
            <button type="button" onClick={addStation}>
              Add station
            </button>
          </div>
        </section>

        <section className={section === "about" ? "setup-section is-active" : "setup-section"}>
          <h2 className="setup-section-title">About &amp; updates</h2>
          <p className="setup-section-sub">The installed version and any newer release ready to install.</p>
          <Updates />
        </section>

        {failure && (
          <p className="failure" role="alert">
            {failure}
          </p>
        )}
        </form>
        </div>

        {section !== "about" && (
          <div className="savebar">
            <button className="primary" type="submit" form="setup-form" disabled={busy}>
              {busy ? "Saving…" : "Save setup"}
            </button>
            {isDirty ? (
              <span className="unsaved">Unsaved changes</span>
            ) : (
              <span className="savebar-note">Everything is saved</span>
            )}
          </div>
        )}
      </div>

      <dialog ref={dialogRef} className="confirm" aria-labelledby="direction-title" onCancel={() => setRoleChange(null)}>
        <h2 id="direction-title">Change a gate direction?</h2>
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

/**
 * The device a station should hold after its kind changes. A real reader is
 * refitted to the new kind (a desk reader is on USB, a gate reader on the
 * network), so the wrong traffic never goes to the wrong hardware.
 */
function asKindDevice(
  station: StationConfig,
  kind: StationKind,
  forceSimulator = false,
): DeviceChoice {
  if (!forceSimulator) {
    const hardware =
      station.device.type === "ecrfid_desk" || station.device.type === "ecrfid_gate"
        ? station.device.hardware
        : null;
    if (hardware) return realDevice(kind, fitHardware(kind, hardware));
    if (kind === "desk" && station.device.type === "sim_desk") return station.device;
  }
  if (!SIMULATOR) return realDevice(kind, defaultHardware(kind));
  return kind === "gate" ? asSimGate(station) : { type: "sim_desk" };
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

/**
 * Checks once when the page opens, quietly: no internet at a venue is normal.
 * The button checks again and says what it found.
 */
function Updates() {
  const [info, setInfo] = useState<UpdateView | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    updateCheck().then(setInfo, () => {});
  }, []);

  const check = async () => {
    setBusy(true);
    setMessage(null);
    try {
      const next = await updateCheck();
      setInfo(next);
      if (!next.available) setMessage(`RfiDex ${next.current} is the latest version.`);
    } catch (problem) {
      setMessage(errorText(problem));
    } finally {
      setBusy(false);
    }
  };

  const install = async () => {
    setBusy(true);
    setMessage("Downloading the update. RfiDex will close and reopen by itself…");
    try {
      await updateInstall();
    } catch (problem) {
      setMessage(errorText(problem));
      setBusy(false);
    }
  };

  return (
    <div className={info?.available ? "updates has-update" : "updates"} aria-live="polite">
      {info?.available ? (
        <p>
          <strong>RfiDex {info.available} is ready to install</strong>
          <span> · you have {info.current}. Setup and waiting scans are kept.</span>
        </p>
      ) : (
        <p>RfiDex {info?.current ?? ""}</p>
      )}
      {message && <p className="update-message">{message}</p>}
      {info?.available ? (
        <button className="primary" type="button" onClick={() => void install()} disabled={busy}>
          {busy ? "Updating…" : "Update now"}
        </button>
      ) : (
        <button type="button" onClick={() => void check()} disabled={busy}>
          {busy ? "Checking…" : "Check for updates"}
        </button>
      )}
    </div>
  );
}
