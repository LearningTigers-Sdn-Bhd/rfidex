// Real-reader fields for one station.
//
// Everything here is a control: Rust validates what is saved and writes every
// outcome message. Nothing in this file decides whether a reader works, and a
// successful test never claims a reader is verified.
import { useRef, useState } from "react";
import {
  EnumerationKind,
  HardwareConfig,
  HardwareTestAction,
  HardwareTestView,
  SdkConnection,
  StationConfig,
  errorText,
  hardwareEnumerate,
  hardwareTest,
} from "./api";

const DEFAULT_TIMEOUT_MS = 2000;

type Tcp = Extract<HardwareConfig, { transport: "ec_v19_plain_tcp" }>;
type Sdk = Extract<HardwareConfig, { transport: "ecrfid_sdk" }>;

/** The candidate TCP profile, the one an operator is most likely to start with. */
export const defaultHardware = (): HardwareConfig => ({
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
}

export default function HardwareFields({ station, dirty, onChange }: Props) {
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
      <div className="row">
        <label>
          Reader type
          <select
            value={hardware.transport}
            onChange={(e) => {
              const wanted = e.target.value as HardwareConfig["transport"];
              if (wanted === hardware.transport) return;
              // The DLL path is the field an operator retypes most, so it is
              // the one thing carried across a change of type.
              onChange(
                wanted === "ecrfid_sdk"
                  ? defaultSdk(hardware.transport === "ecrfid_sdk" ? hardware.dll_path : "")
                  : defaultHardware(),
              );
            }}
          >
            <option value="ec_v19_plain_tcp">EC v1.9 over TCP</option>
            <option value="ecrfid_sdk">ECRFID SDK</option>
          </select>
          <small className="field-help">
            {hardware.transport === "ec_v19_plain_tcp"
              ? "Unverified candidate: built from the vendor guide, with no capture confirming it yet."
              : "The vendor library, over USB, a serial port or the network."}
          </small>
        </label>
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
      </div>

      {hardware.transport === "ec_v19_plain_tcp" ? (
        <TcpFields value={hardware} onChange={onChange} />
      ) : (
        <SdkFields value={hardware} onChange={onChange} />
      )}

      <div className="row">
        <div className="actions">
          <button type="button" onClick={() => void test("connect")} disabled={busy || dirty}>
            {dirty ? "Save setup to test this reader" : "Test connection"}
          </button>
          <button type="button" onClick={() => void test("read_tags")} disabled={busy || dirty}>
            {dirty ? "Save setup to read stickers" : "Read stickers"}
          </button>
        </div>
      </div>

      <p className="field-help">
        Testing a reader is never a verification. Only the physical acceptance
        checks on the real hardware can mark a reader verified.
      </p>
      {dirty && (
        <p className="field-help">
          These edits take effect after the setup is saved.
        </p>
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
      <label>
        Address
        <input
          value={value.address}
          onChange={(e) => set({ address: e.target.value })}
          placeholder="192.168.1.20:6688"
        />
        <small className="field-help">The reader's own IP address and port.</small>
      </label>
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
  onChange,
}: {
  value: Sdk;
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
            placeholder="C:\Program Files\RFiDex\ECRFID.dll"
          />
          <small className="field-help">
            The full path to the vendor library on this computer.
          </small>
        </label>
        <label>
          Connection
          <select
            value={connection.kind}
            onChange={(e) =>
              setConnection(defaultConnection(e.target.value as SdkConnection["kind"]))
            }
          >
            <option value="hid">USB HID</option>
            <option value="com">Serial port</option>
            <option value="net">Network</option>
          </select>
        </label>
        <label>
          Model
          <input
            value={connection.model}
            onChange={(e) => setConnection({ ...connection, model: e.target.value })}
            placeholder="EC1101"
          />
          <small className="field-help">
            The model printed on the reader. Nothing is assumed from the reader
            list.
          </small>
        </label>
      </div>

      {connection.kind === "hid" && (
        <div className="row">
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
          <label>
            Address mode
            <input
              type="number"
              min={0}
              value={connection.address_mode}
              onChange={(e) => setConnection({ ...connection, address_mode: Number(e.target.value) })}
            />
            <small className="field-help">The vendor HID address mode; the demo uses 1.</small>
          </label>
          <label>
            Exclusive access
            <input type="number" value={connection.exclusive} readOnly />
            <small className="field-help">This app reserves one reader for one station.</small>
          </label>
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

      {connection.kind === "net" && (
        <div className="row">
          <label>
            Interface
            <input
              value={connection.interface}
              onChange={(e) =>
                setConnection({ ...connection, interface: e.target.value })
              }
              placeholder="192.168.1.10"
            />
          </label>
          <label>
            Reader address
            <input
              value={connection.address}
              onChange={(e) =>
                setConnection({ ...connection, address: e.target.value })
              }
              placeholder="192.168.1.20:6688"
            />
          </label>
        </div>
      )}

      <div className="row">
        <label>
          Inventory mode
          <input
            type="number"
            min={0}
            max={255}
            value={value.inventory_mode}
            onChange={(e) => set({ inventory_mode: Number(e.target.value) })}
          />
          <small className="field-help">
            The vendor's mode byte. 4 is the value the vendor demo uses for
            ISO15693.
          </small>
        </label>
      </div>
      <div className="row">
        <label>
          Write acceptance
          <input readOnly value={value.write_verified ? "Write enabled in saved profile" : "Write disabled"} />
          <small className="field-help">This flag is not proof of physical acceptance. Setup cannot enable writing.</small>
        </label>
      </div>
      <div className="actions">
        <button type="button" onClick={() => void look()} disabled={busy}>
          Look for readers
        </button>
      </div>

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
