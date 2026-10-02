// Real-reader fields for one station.
//
// Everything here is a control: Rust validates what is saved and writes every
// outcome message. Nothing in this file decides whether a reader works, and a
// successful test never claims a reader is verified.
import { useEffect, useRef, useState } from "react";
import StickerTest from "./StickerTest";
import {
  AppView,
  EnumerationKind,
  HardwareConfig,
  HardwareTestAction,
  HardwareTestView,
  SdkConnection,
  StationConfig,
  StationKind,
  errorText,
  hardwareDiscover,
  hardwareEnumerate,
  hardwareTest,
} from "./api";

const DEFAULT_TIMEOUT_MS = 2000;

type Tcp = Extract<HardwareConfig, { transport: "ec_v19_plain_tcp" }>;
type Sdk = Extract<HardwareConfig, { transport: "ecrfid_sdk" }>;

/** The unverified plain-TCP gate profile, reachable only from a gate's
 * advanced settings. */
const defaultTcp = (): HardwareConfig => ({
  transport: "ec_v19_plain_tcp",
  address: "192.168.1.20:6688",
  bus_address: 255,
  antenna_byte: false,
  timeout_ms: DEFAULT_TIMEOUT_MS,
});

export const defaultSdk = (dllPath: string): Sdk => ({
  transport: "ecrfid_sdk",
  dll_path: dllPath,
  connection: { kind: "hid", model: "", path: "", address_mode: 1, exclusive: 1 },
  inventory_mode: 4,
  timeout_ms: DEFAULT_TIMEOUT_MS,
});

/** A desk reader is on USB; a gate reader is on the network. */
const connectionFor = (kind: StationKind): SdkConnection["kind"] =>
  kind === "desk" ? "hid" : "net";

/** A new real reader for a station of this kind. */
export const defaultHardware = (kind: StationKind): HardwareConfig => ({
  ...defaultSdk(""),
  connection: defaultConnection(connectionFor(kind)),
});

/** The reader settings fitted to a station kind: a desk gets a USB reader and
 * a gate a network one, keeping the DLL path. Anything else is replaced. */
export const fitHardware = (kind: StationKind, hardware: HardwareConfig): HardwareConfig => {
  if (hardware.transport !== "ecrfid_sdk") {
    return kind === "gate" ? hardware : defaultHardware(kind);
  }
  return hardware.connection.kind === connectionFor(kind)
    ? hardware
    : { ...hardware, connection: defaultConnection(connectionFor(kind)) };
};

const defaultConnection = (kind: SdkConnection["kind"]): SdkConnection => {
  switch (kind) {
    case "com":
      return {
        kind: "com",
        model: "",
        port: "",
        baud: 38400,
        frame: "8E1",
        bus_address: 255,
      };
    case "net":
      return { kind: "net", model: "", interface: "", address: "" };
    default:
      return { kind: "hid", model: "", path: "", address_mode: 1, exclusive: 1 };
  }
};

interface Props {
  station: StationConfig;
  /** True when the screen still holds edits that have not been saved. */
  dirty: boolean;
  onChange: (hardware: HardwareConfig) => void;
  /** Turning sticker writing on or off saves the setup and restarts stations. */
  onSaved: (view: AppView) => void;
}

export default function HardwareFields({ station, dirty, onChange, onSaved }: Props) {
  // Track station identity, not just serialized values: edit-then-revert must
  // still invalidate a result returned by an earlier reader request.
  const revision = useRef({ station, dirty, number: 0 });
  if (revision.current.station !== station || revision.current.dirty !== dirty) {
    revision.current = { station, dirty, number: revision.current.number + 1 };
  }
  const [tested, setTested] = useState<
    { revision: number; action: HardwareTestAction; view: HardwareTestView } | null
  >(null);
  const [busy, setBusy] = useState(false);
  const [confirmingClear, setConfirmingClear] = useState(false);
  const clearDialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = clearDialog.current;
    if (!dialog) return;
    if (confirmingClear && !dialog.open) dialog.showModal();
    else if (!confirmingClear && dialog.open) dialog.close();
  }, [confirmingClear]);
  const [failure, setFailure] = useState<{ revision: number; message: string } | null>(null);

  const device = station.device;
  if (device.type !== "ecrfid_desk" && device.type !== "ecrfid_gate") return null;
  const hardware = device.hardware;

  const test = async (action: HardwareTestAction) => {
    if (dirty || busy) return;
    const current = revision.current.number;
    setBusy(true);
    setFailure(null);
    setTested(null);
    try {
      const view = await hardwareTest(station.id, action);
      if (revision.current.number === current && !revision.current.dirty) {
        setTested({ revision: current, action, view });
      }
    } catch (e) {
      if (revision.current.number === current && !revision.current.dirty) {
        setFailure({ revision: current, message: errorText(e) });
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="hardware-field">
      {hardware.transport === "ec_v19_plain_tcp" ? (
        <TcpFields value={hardware} onChange={onChange} />
      ) : (
        <SdkFields value={hardware} kind={station.kind} onChange={onChange} />
      )}

      <details className="advanced">
        <summary>Advanced reader settings</summary>
        <div className="advanced-body">
          <p className="field-help">
            Leave these alone unless your RFID supplier tells you otherwise.
          </p>
          <div className="row">
            {station.kind === "gate" && (
              <label>
                Reader type
                <select
                  value={hardware.transport}
                  onChange={(e) => {
                    const wanted = e.target.value as HardwareConfig["transport"];
                    if (wanted === hardware.transport) return;
                    onChange(
                      wanted === "ecrfid_sdk"
                        ? defaultHardware(station.kind)
                        : defaultTcp(),
                    );
                  }}
                >
                  <option value="ecrfid_sdk">ECRFID SDK</option>
                  <option value="ec_v19_plain_tcp">EC v1.9 over TCP (unverified)</option>
                </select>
                <small className="field-help">
                  {hardware.transport === "ec_v19_plain_tcp"
                    ? "Unverified candidate: built from the vendor guide, with no capture confirming it yet."
                    : "The vendor library. Use this unless told otherwise."}
                </small>
              </label>
            )}
            <label>
              Reader timeout (ms)
              <input
                type="number"
                min={100}
                max={10000}
                step={100}
                value={hardware.timeout_ms}
                onChange={(e) =>
                  onChange({ ...hardware, timeout_ms: Number(e.target.value) })
                }
              />
              <small className="field-help">
                The most one reader call may take, between 100 and 10000.
              </small>
            </label>
            {hardware.transport === "ecrfid_sdk" && (
              <label>
                Inventory mode
                <input
                  type="number"
                  min={0}
                  max={255}
                  value={hardware.inventory_mode}
                  onChange={(e) =>
                    onChange({ ...hardware, inventory_mode: Number(e.target.value) })
                  }
                />
                <small className="field-help">
                  The vendor's mode byte. 4 is the value the vendor demo uses for
                  ISO15693.
                </small>
              </label>
            )}
            {hardware.transport === "ecrfid_sdk" && hardware.connection.kind === "hid" && (
              <label>
                Address mode
                <input
                  type="number"
                  min={0}
                  value={hardware.connection.address_mode}
                  onChange={(e) =>
                    hardware.connection.kind === "hid" &&
                    onChange({
                      ...hardware,
                      connection: { ...hardware.connection, address_mode: Number(e.target.value) },
                    })
                  }
                />
                <small className="field-help">The vendor HID address mode; the demo uses 1.</small>
              </label>
            )}
          </div>
        </div>
      </details>


      {dirty ? (
        <p className="caution" role="status">
          Unsaved changes. Press <strong>Save setup</strong> to test this reader.
        </p>
      ) : (
        <div className="row">
          <div className="actions">
            <button type="button" onClick={() => void test("connect")} disabled={busy}>
              Test connection
            </button>
            <button type="button" onClick={() => void test("read_tags")} disabled={busy}>
              Read stickers
            </button>
            {station.kind === "gate" && (
              <button type="button" onClick={() => void test("gate_records")} disabled={busy}>
                Read gate records
              </button>
            )}
            {station.kind === "gate" && (
              <button type="button" onClick={() => setConfirmingClear(true)} disabled={busy}>
                Clear gate records
              </button>
            )}
          </div>
          {station.kind === "gate" && (
            <small className="field-help">
              Read gate records takes the next pass off the gate and does not save it. Use it for
              setup only, never while guests are passing. Clear gate records wipes the passes stored on the
              gate for good.
            </small>
          )}
        </div>
      )}
      {tested?.revision === revision.current.number && !dirty && (
        <div className={tested.view.ok ? "note" : "failure"} role="status">
          <p>{tested.view.message}</p>
          {tested.view.uid_raw_hex.length > 0 && (
            <ul className="found-readers">
              {tested.view.uid_raw_hex.map((uid) => (
                <li key={uid}>
                  <code>{uid}</code>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
      {failure?.revision === revision.current.number && !dirty && (
        <p className="failure" role="status">
          {failure.message}
        </p>
      )}

      <dialog
        ref={clearDialog}
        className="confirm"
        aria-labelledby="clear-records-title"
        onCancel={() => setConfirmingClear(false)}
      >
        <h2 id="clear-records-title">Clear all gate records?</h2>
        <p>
          This wipes every pass stored on {station.name || "this gate"}. Passes the gate has not
          handed over yet are lost for good. This cannot be undone.
        </p>
        <div className="dialog-actions">
          <button type="button" onClick={() => setConfirmingClear(false)}>
            Cancel — keep the records
          </button>
          <button
            type="button"
            className="danger"
            onClick={() => {
              setConfirmingClear(false);
              void test("clear_gate_records");
            }}
            disabled={busy}
          >
            Clear them for good
          </button>
        </div>
      </dialog>

      {device.type === "ecrfid_desk" && hardware.transport === "ecrfid_sdk" && !dirty && (
        <StickerTest stationId={station.id} onSaved={onSaved} />
      )}
    </div>
  );
}

function TcpFields({
  value,
  onChange,
}: {
  value: Tcp;
  onChange: (hardware: HardwareConfig) => void;
}) {
  const set = (update: Partial<Tcp>) => onChange({ ...value, ...update });
  return (
    <div className="row">
      <AddressFields
        address={value.address}
        onChange={(address) => set({ address })}
        ipLabel="Reader IP address"
        ipHelp="The reader's own IP address, for example 192.168.1.20."
      />
      <label>
        Bus address
        <input
          type="number"
          min={0}
          max={255}
          value={value.bus_address}
          onChange={(e) => set({ bus_address: Number(e.target.value) })}
        />
        <small className="field-help">
          255 is the broadcast address for a single reader.
        </small>
      </label>
      <label className="checkbox">
        <input
          type="checkbox"
          checked={value.antenna_byte}
          onChange={(e) => set({ antenna_byte: e.target.checked })}
        />
        The reader reports an antenna address with each sticker.
      </label>
    </div>
  );
}

function SdkFields({
  value,
  kind,
  onChange,
}: {
  value: Sdk;
  kind: StationKind;
  onChange: (hardware: HardwareConfig) => void;
}) {
  const revision = useRef({ value, number: 0 });
  if (revision.current.value !== value) {
    revision.current = { value, number: revision.current.number + 1 };
  }
  const [found, setFound] = useState<
    { revision: number; kind: EnumerationKind; entries: string[] } | null
  >(null);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<{ revision: number; message: string } | null>(null);

  const set = (update: Partial<Sdk>) => onChange({ ...value, ...update });
  const setConnection = (connection: SdkConnection) =>
    onChange({ ...value, connection });
  const connection = value.connection;

  const [gates, setGates] = useState<{ revision: number; entries: string[] } | null>(null);

  const findGates = async () => {
    if (busy || connection.kind !== "net") return;
    const current = revision.current.number;
    setBusy(true);
    setFailure(null);
    setGates(null);
    try {
      const entries = await hardwareDiscover(value.dll_path, connection.interface);
      if (revision.current.number === current) setGates({ revision: current, entries });
    } catch (e) {
      if (revision.current.number === current) {
        setFailure({ revision: current, message: errorText(e) });
      }
    } finally {
      setBusy(false);
    }
  };

  const look = async () => {
    if (busy) return;
    const current = revision.current.number;
    const kind = connection.kind;
    setBusy(true);
    setFailure(null);
    setFound(null);
    try {
      const entries = await hardwareEnumerate(value.dll_path, kind);
      if (revision.current.number === current) setFound({ revision: current, kind, entries });
    } catch (e) {
      if (revision.current.number === current) {
        setFailure({ revision: current, message: errorText(e) });
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <div className="row">
        <label>
          ECRFID.dll
          <input
            value={value.dll_path}
            onChange={(e) => set({ dll_path: e.target.value })}
            placeholder="Empty: ECRFID.dll in the RfiDex folder"
          />
          <small className="field-help">
            Leave empty if ECRFID.dll is copied into the RfiDex install folder.
            Otherwise enter its full path on this computer.
          </small>
        </label>
        <label>
          Model
          <input
            value={connection.model}
            onChange={(e) => setConnection({ ...connection, model: e.target.value })}
            placeholder={kind === "desk" ? "EC1101" : ""}
          />
          <small className="field-help">
            The model printed on the reader. Nothing is assumed from the reader
            list.
          </small>
        </label>
        {connection.kind === "hid" && (
          <label>
            Device path
            <input
              value={connection.path}
              onChange={(e) => setConnection({ ...connection, path: e.target.value })}
              placeholder="\\?\hid#vid_0483&pid_5750#7&1c2f0b3&0&0000"
            />
            <small className="field-help">
              Pick the reader from the list. The first device is never chosen for
              you.
            </small>
          </label>
        )}
        {connection.kind === "net" && (
          <label>
            Interface
            <input
              value={connection.interface}
              onChange={(e) =>
                setConnection({ ...connection, interface: e.target.value })
              }
              placeholder="192.168.1.10"
            />
            <small className="field-help">
              The network card the gate is cabled to.
            </small>
          </label>
        )}
      </div>

      {connection.kind === "net" && (
        <div className="row net-address-row">
          <AddressFields
            address={connection.address}
            onChange={(address) => setConnection({ ...connection, address })}
            ipLabel="Gate IP address"
            ipHelp="The gate's own IP address, for example 192.168.1.222. Find gates can fill it in."
          />
          <div className="actions">
            <button type="button" onClick={() => void findGates()} disabled={busy}>
              Find gates
            </button>
            <button type="button" onClick={() => void look()} disabled={busy}>
              Look for network cards
            </button>
          </div>
        </div>
      )}

      {connection.kind === "com" && (
        <div className="row">
          <label>
            Port
            <input
              value={connection.port}
              onChange={(e) => setConnection({ ...connection, port: e.target.value })}
              placeholder="COM3"
            />
          </label>
          <label>
            Baud
            <input
              type="number"
              min={1}
              value={connection.baud}
              onChange={(e) =>
                setConnection({ ...connection, baud: Number(e.target.value) })
              }
            />
          </label>
          <label>
            Frame
            <input
              value={connection.frame}
              onChange={(e) => setConnection({ ...connection, frame: e.target.value })}
              placeholder="8E1"
            />
          </label>
          <label>
            Bus address
            <input
              type="number"
              min={0}
              max={255}
              value={connection.bus_address}
              onChange={(e) => setConnection({ ...connection, bus_address: Number(e.target.value) })}
            />
          </label>
        </div>
      )}

      {gates?.revision === revision.current.number && (
        <>
          <p className="note" role="status">
            {gates.entries.length === 0
              ? "No gate answered on this network card. Check the gate is powered and on this cable, and that Interface is the Ethernet entry."
              : `${gates.entries.length} gate${gates.entries.length === 1 ? "" : "s"} found.`}
          </p>
          {gates.entries.length > 0 && connection.kind === "net" && (
            <ul className="found-readers">
              {gates.entries.map((entry) => (
                <li key={entry}>
                  <code>{entry}</code>
                  <button
                    type="button"
                    onClick={() =>
                      setConnection({
                        ...connection,
                        address: /address=([^;]+)/.exec(entry)?.[1] ?? connection.address,
                      })
                    }
                  >
                    Use this one
                  </button>
                </li>
              ))}
            </ul>
          )}
        </>
      )}

      {kind === "desk" && (
        <div className="row">
          <label>
            Write acceptance
            <input readOnly value={value.write_verified ? "Write enabled in saved profile" : "Write disabled"} />
            <small className="field-help">Turned on or off only by the sticker write test below, after a disposable sticker passes.</small>
          </label>
        </div>
      )}
      {connection.kind !== "net" && (
        <div className="actions">
          <button type="button" onClick={() => void look()} disabled={busy}>
            {kind === "desk" ? "Look for readers" : "Look for network cards"}
          </button>
        </div>
      )}

      {found?.revision === revision.current.number && (
        <p className="note" role="status">
          {found.entries.length === 0
            ? "The library found no devices."
            : `${found.entries.length} device${found.entries.length === 1 ? "" : "s"} found.`}
        </p>
      )}
      {found?.revision === revision.current.number && found.entries.length > 0 && (
        <ul className="found-readers">
          {found.entries.map((entry) => (
            <li key={entry}>
              <code>{entry}</code>
              <button
                type="button"
                onClick={() => {
                  if (found.kind === "hid" && connection.kind === "hid") {
                    setConnection({ ...connection, path: entry });
                  } else if (found.kind === "com" && connection.kind === "com") {
                    setConnection({ ...connection, port: entry });
                  } else if (found.kind === "net" && connection.kind === "net") {
                    setConnection({ ...connection, interface: entry });
                  }
                }}
              >
                Use this one
              </button>
            </li>
          ))}
        </ul>
      )}
      {failure?.revision === revision.current.number && (
        <p className="failure" role="status">
          {failure.message}
        </p>
      )}
    </>
  );
}

const DEFAULT_PORT = "6688";

/** The saved address is `IP:port`; the screen edits the two halves apart. */
function splitAddress(address: string): { ip: string; port: string } {
  const at = address.lastIndexOf(":");
  return at < 0
    ? { ip: address, port: DEFAULT_PORT }
    : { ip: address.slice(0, at), port: address.slice(at + 1) };
}

function AddressFields({
  address,
  onChange,
  ipLabel,
  ipHelp,
}: {
  address: string;
  onChange: (address: string) => void;
  ipLabel: string;
  ipHelp: string;
}) {
  const { ip, port } = splitAddress(address);
  // Untouched, both halves stay empty, so a new station still asks for the IP.
  const join = (nextIp: string, nextPort: string) =>
    nextIp === "" && nextPort === DEFAULT_PORT ? "" : `${nextIp}:${nextPort}`;
  return (
    <>
      <label>
        {ipLabel}
        <input
          value={ip}
          onChange={(e) => onChange(join(e.target.value.trim(), port))}
          placeholder="192.168.1.222"
        />
        <small className="field-help">{ipHelp}</small>
      </label>
      <label className="port-field">
        Port
        <input
          type="number"
          min={1}
          max={65535}
          value={port}
          onChange={(e) => onChange(join(ip, e.target.value))}
        />
        <small className="field-help">Default 6688. Change only if necessary.</small>
      </label>
    </>
  );
}
