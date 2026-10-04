import { useEffect, useRef, useState } from "react";
import { badgeGet, badgeImport, badgePreview, badgePrint, badgeSave, badgeTestPrint, badgeTicketTypes, errorText } from "./api";
import type { BadgeLayout, BadgeSettings, BadgeSettingsView, BadgeTicket } from "./api";
import {
  addCustom, editCustom, FIELD_LABEL, isBold, isOn, MAX_CUSTOM_FIELDS, move, newCustomId, offsetOf,
  PAPER_PRESETS, paperPresetId, removeCustom, resetAdjustments, rows, scaleOf, setBold, setOffset, setOn,
  setPaper, setScale, setVertical,
} from "./badge-layout";

const sample: BadgeTicket = {
  ticket_id: "00000000-0000-0000-0000-000000000001",
  name: "Tan Wei Ming 陈伟明",
  company: "Borneo Expo",
  title: "Guest",
  country: "Malaysia",
  table_no: "12",
  ticket_type: "VIP",
  role: "Delegate",
  custom: {},
};

type Section = "printer" | "fields" | "layouts" | "manual" | "import";

const SECTIONS: readonly (readonly [Section, string])[] = [
  ["printer", "Printer & paper"],
  ["fields", "Badge fields"],
  ["layouts", "Saved layouts"],
  ["manual", "Manual print"],
  ["import", "Import"],
];

/** Used until this PC has loaded the event's own ticket types. */
const COMMON_TYPES = ["Visitor", "Delegate", "Speaker", "Moderator", "Student", "Staff", "Organizer", "Exhibitor", "VIP", "VVIP"];
const OTHER = "\u0000other";

/** Ticket type: a pick list of the event's types, with Other for anything else. */
function TicketTypeField({ id, label, value, types, onChange }: {
  id: string; label: string; value: string; types: string[]; onChange: (next: string) => void;
}) {
  const list = types.length > 0 ? types : COMMON_TYPES;
  const [other, setOther] = useState(false);
  const typed = other || (value !== "" && !list.includes(value));
  return (
    <div className="field">
      <label htmlFor={id}>{label}</label>
      <select
        id={id}
        value={typed ? OTHER : value}
        onChange={(e) => {
          if (e.target.value === OTHER) { setOther(true); return; }
          setOther(false);
          onChange(e.target.value);
        }}
      >
        <option value="">Choose a ticket type…</option>
        {list.map((name) => <option key={name} value={name}>{name}</option>)}
        <option value={OTHER}>Other…</option>
      </select>
      {typed && <input aria-label={`${label}, typed`} className="printer-other" value={value} placeholder="Type the ticket type" onChange={(e) => onChange(e.target.value)} />}
      <small className="field-help">{types.length > 0 ? "The event's ticket types and the ones you added under Badge types." : "Common types. The event's own types appear once this PC has loaded its tickets. Add your own under Badge types."}</small>
    </div>
  );
}

/** Which typed guest detail each badge field prints. */
type GuestKey = Exclude<keyof BadgeTicket, "custom">;
const GUEST_KEY: Record<string, GuestKey> = {
  name: "name",
  role: "ticket_type",
  ticket_role: "role",
  company: "company",
  title: "title",
  country: "country",
  table_no: "table_no",
  qr: "ticket_id",
};

const ICON = {
  printer: ["M6 9V3h12v6", "M6 18H4a2 2 0 0 1-2-2v-5a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v5a2 2 0 0 1-2 2h-2", "M6 14h12v7H6z"],
  paper: ["M6 3h9l4 4v14H6z", "M15 3v4h4"],
  fields: ["M4 6h16", "M4 12h16", "M4 18h10"],
  eye: ["M2 12s4-7 10-7 10 7 10 7-4 7-10 7S2 12 2 12z", "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6z"],
  layers: ["M12 3 3 8l9 5 9-5z", "M3 13l9 5 9-5"],
  import: ["M12 3v12", "M7 10l5 5 5-5", "M4 21h16"],
} as const;

/** A card head in the same form as the station cards in Setup. */
function CardHead({ icon, title, chip }: { icon: readonly string[]; title: string; chip?: string }) {
  return (
    <header className="station-card-head">
      <span className="station-card-icon" aria-hidden="true">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
          {icon.map((d) => <path key={d} d={d} />)}
        </svg>
      </span>
      <h3 className="printer-card-title">{title}</h3>
      {chip && <span className="station-card-chip">{chip}</span>}
    </header>
  );
}

/**
 * A number with − and + buttons at each end of a slider, and a box to type it.
 * The typed text is kept while the box has focus, so "-" and "1." can be typed.
 */
function Stepper({ id, label, value, min, max, step, unit, disabled, onChange }: {
  id: string; label: string; value: number; min: number; max: number; step: number;
  unit?: string; disabled?: boolean; onChange: (next: number) => void;
}) {
  const decimals = (String(step).split(".")[1] ?? "").length;
  const clamp = (n: number) => Math.min(max, Math.max(min, Number(n.toFixed(decimals))));
  const [text, setText] = useState(String(value));
  const focused = useRef(false);
  useEffect(() => { if (!focused.current) setText(String(value)); }, [value]);
  const commit = (n: number) => { if (Number.isFinite(n)) onChange(clamp(n)); };
  return (
    <div className="stepper">
      <div className="stepper-head">
        <label htmlFor={id}>{label}</label>
        <span className="stepper-value">
          <input
            id={id}
            type="number"
            min={min}
            max={max}
            step={step}
            value={text}
            disabled={disabled}
            onFocus={() => { focused.current = true; }}
            onBlur={() => { focused.current = false; setText(String(value)); }}
            onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); }}
            onChange={(e) => {
              setText(e.target.value);
              const n = parseFloat(e.target.value);
              if (e.target.value !== "" && Number.isFinite(n) && n >= min && n <= max) onChange(n);
            }}
          />
          {unit && <em>{unit}</em>}
        </span>
      </div>
      <div className="stepper-track">
        <button type="button" aria-label={`Decrease ${label}`} disabled={disabled || value <= min} onClick={() => commit(value - step)}>−</button>
        <input type="range" aria-label={`${label} slider`} min={min} max={max} step={step} value={value} disabled={disabled} onChange={(e) => commit(Number(e.target.value))} />
        <button type="button" aria-label={`Increase ${label}`} disabled={disabled || value >= max} onClick={() => commit(value + step)}>+</button>
      </div>
    </div>
  );
}

export function Printer() {
  const [view, setView] = useState<BadgeSettingsView | null>(null);
  const [settings, setSettings] = useState<BadgeSettings | null>(null);
  const [ticket, setTicket] = useState(sample);
  const [image, setImage] = useState<string | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [adjust, setAdjust] = useState(false);
  const [section, setSection] = useState<Section>("printer");
  const [presetName, setPresetName] = useState("");
  const [importConfirm, setImportConfirm] = useState(false);
  const [types, setTypes] = useState<string[]>([]);
  const [newType, setNewType] = useState("");
  const request = useRef(0);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    void badgeGet().then((next) => {
      if (mounted.current) { setView(next); setSettings(next.settings); }
    }).catch((e) => { if (mounted.current) setFailure(errorText(e)); });
    return () => { mounted.current = false; };
  }, []);

  useEffect(() => {
    void badgeTicketTypes().then((next) => { if (mounted.current) setTypes(next); }).catch(() => {});
  }, []);

  const layout = settings?.layout;
  useEffect(() => {
    if (!layout) return;
    const id = ++request.current;
    setPreviewError(null);
    const timer = window.setTimeout(() => {
      void badgePreview(layout, ticket).then((next) => {
        if (mounted.current && id === request.current) { setImage(next); setPreviewError(null); }
      }).catch((e) => {
        if (mounted.current && id === request.current) { setImage(null); setPreviewError(errorText(e)); }
      });
    }, 250);
    return () => { window.clearTimeout(timer); request.current++; };
  }, [layout, ticket]);

  const run = async (action: () => Promise<void>) => {
    setBusy(true); setFailure(null); setNotice(null);
    try { await action(); } catch (e) { if (mounted.current) setFailure(errorText(e)); }
    finally { if (mounted.current) setBusy(false); }
  };
  const accept = (next: BadgeSettingsView) => {
    if (mounted.current) { setView(next); setSettings(next.settings); }
  };
  const save = async () => {
    if (!settings) return;
    const next = await badgeSave(settings); accept(next); return next;
  };

  if (!view || !settings || !layout) {
    return (
      <div className="subpage setup-page printer-page">
        <div className="subpage-content setup-content">
          <header className="setup-head"><div><p className="eyebrow">RfiDex / Badges</p><h1>Printer</h1></div></header>
          {failure ? <p className="failure" role="alert">{failure}</p> : <p className="hint">Loading printer settings…</p>}
          <div className="actions"><button type="button" onClick={() => void run(async () => accept(await badgeGet()))}>Reload</button></div>
        </div>
      </div>
    );
  }

  const dirty = view.warning !== null || JSON.stringify(settings) !== JSON.stringify(view.settings);
  const change = (patch: Partial<BadgeSettings>) => setSettings((current) => current ? { ...current, ...patch } : current);
  const editLayout = (next: BadgeLayout) => change({ layout: next });
  const names = [...new Set([...view.printers.names, ...(settings.printer ? [settings.printer] : [])])];
  const selectedPreset = settings.active_preset;
  const preset = paperPresetId(layout);
  const methodName = view.provider.replace("Badge printing: ", "");
  const labelOf = (id: string) => layout.custom_fields[id]?.label ?? FIELD_LABEL[id] ?? id;
  const printerStatus = (() => {
    if (!view.printers.supported) return { ok: false, text: "Printing works on Windows only. Open this page on the desk PC to print." };
    const chosen = settings.printer || view.printers.default;
    if (!chosen) return { ok: false, text: "No printer is set up on this PC. Add one in Windows, then reopen this page." };
    const listed = settings.printer ? view.printers.names.includes(settings.printer) : true;
    return listed
      ? { ok: true, text: `Printing to “${chosen}”. Windows lists this printer; a test badge confirms it is connected and the paper is right.` }
      : { ok: false, text: `“${chosen}” is not in this PC's Windows printer list. Pick another in Printer & paper.` };
  })();
  // Only the fields ticked in Badge fields, in the same order and under the same
  // names, so what is typed here is exactly what the badge can show.
  const typeOptions = [...types, ...settings.badge_types.filter((n) => !types.some((t) => t.toLowerCase() === n.toLowerCase()))];
  const guestFields = layout.elements.length === 0 ? (
    <p className="hint">Tick at least one field in Badge fields first.</p>
  ) : (
    <div className="station-fields">
      {layout.elements.map((id) => {
        const custom = layout.custom_fields[id];
        if (custom) {
          return (
            <div className="field" key={id}>
              <label htmlFor={`guest-${id}`}>{custom.label}</label>
              <input id={`guest-${id}`} value={ticket.custom[id] ?? ""} onChange={(e) => setTicket((current) => ({ ...current, custom: { ...current.custom, [id]: e.target.value } }))} />
            </div>
          );
        }
        const key = GUEST_KEY[id];
        if (!key) return null;
        if (id === "role") {
          return <TicketTypeField key={id} id={`guest-${id}`} label={labelOf(id)} value={ticket.ticket_type} types={typeOptions} onChange={(next) => setTicket((current) => ({ ...current, ticket_type: next }))} />;
        }
        return (
          <div className="field" key={id}>
            <label htmlFor={`guest-${id}`}>{labelOf(id)}</label>
            <input id={`guest-${id}`} value={ticket[key]} onChange={(e) => setTicket((current) => ({ ...current, [key]: e.target.value }))} />
            {id === "qr" && <small className="field-help">What the QR code holds. Use the guest's ticket ID so the badge scans.</small>}
          </div>
        );
      })}
    </div>
  );
  const sectionClass = (id: Section) => (section === id ? "setup-section is-active" : "setup-section");

  return (
    <div className="subpage setup-page printer-page">
      <nav className="subnav" aria-label="Printer sections">
        <p className="nav-lbl">Printer</p>
        {SECTIONS.map(([id, label]) => (
          <button
            key={id}
            type="button"
            className={section === id ? "subnav-btn is-active" : "subnav-btn"}
            aria-current={section === id ? "page" : undefined}
            onClick={() => setSection(id)}
          >
            {label}
          </button>
        ))}
      </nav>

      <div className="subpage-content setup-content">
        <header className="setup-head">
          <div>
            <p className="eyebrow">RfiDex / Badges</p>
            <h1>Printer</h1>
          </div>
        </header>
        <p className="hint">
          The printer, paper and badge layout used by built-in printing. Current method:{" "}
          <strong>{methodName}</strong> — change it in Setup → Badge printing. Test print works with either method.
        </p>

        <div className="setup-body">
          {view.warning && <p className="failure" role="alert">{view.warning}</p>}
          {!view.printers.supported && <p className="note">Badge printing works on Windows only. You can still edit layouts and preview them here.</p>}
          {failure && <p className="failure" role="alert">{failure}</p>}
          {notice && <p className="note" role="status">{notice}</p>}

          <div className={section === "manual" ? "printer-workbench is-manual" : "printer-workbench"}>
            <div className="printer-main">
              <section className={sectionClass("printer")}>
                <h2 className="setup-section-title">Printer &amp; paper</h2>
                <p className="setup-section-sub">Which printer prints the badge, and the size of the stock loaded in it.</p>
                <ul className="station-editor">
                  <li className="station-card">
                    <CardHead icon={ICON.printer} title="Printer" chip={view.printers.supported ? "Windows" : "Preview only"} />
                    <div className="station-card-body">
                      <div className="station-fields">
                        <div className="field">
                          <label htmlFor="printer-name">Printer</label>
                          <select id="printer-name" value={settings.printer} disabled={busy} onChange={(e) => change({ printer: e.target.value })}>
                            <option value="">Windows default printer{view.printers.default ? ` — ${view.printers.default}` : ""}</option>
                            {names.map((name) => <option key={name} value={name}>{name}</option>)}
                          </select>
                          <small className="field-help">Leave on the default to use whichever printer Windows has set as its default.</small>
                        </div>
                      </div>
                      <div className="station-cluster">
                        <div className="printer-option">
                          <label className="switch-row">
                            <span>Thin-stroke fix for direct-thermal stock</span>
                            <input type="checkbox" role="switch" className="switch" checked={settings.thermal} disabled={busy} onChange={(e) => change({ thermal: e.target.checked })} />
                          </label>
                          <small className="field-help">Turn on if names print broken or with missing dots on rough or thick tags. Prints bolder.</small>
                        </div>
                      </div>
                    </div>
                  </li>
                  <li className="station-card">
                    <CardHead icon={ICON.paper} title="Paper" chip={`${layout.paper.width_mm} × ${layout.paper.height_mm} mm`} />
                    <div className="station-card-body">
                      <div className="station-fields">
                        <div className="field">
                          <label htmlFor="paper-size">Paper size</label>
                          <select
                            id="paper-size"
                            value={preset}
                            disabled={busy}
                            onChange={(e) => {
                              const chosen = PAPER_PRESETS.find((p) => p.id === e.target.value);
                              editLayout(chosen ? setPaper(layout, chosen.width_mm, chosen.height_mm) : setPaper(layout, 100, 100));
                            }}
                          >
                            {PAPER_PRESETS.map((p) => <option key={p.id} value={p.id}>{p.label}</option>)}
                            <option value="custom">Custom</option>
                          </select>
                        </div>
                        {preset === "custom" && (
                          <>
                            <div className="field">
                              <label htmlFor="paper-width">Width (mm)</label>
                              <input id="paper-width" type="number" min="20" max="500" value={layout.paper.width_mm} disabled={busy} onChange={(e) => editLayout(setPaper(layout, Number(e.target.value), layout.paper.height_mm))} />
                            </div>
                            <div className="field">
                              <label htmlFor="paper-height">Height (mm)</label>
                              <input id="paper-height" type="number" min="20" max="500" value={layout.paper.height_mm} disabled={busy} onChange={(e) => editLayout(setPaper(layout, layout.paper.width_mm, Number(e.target.value)))} />
                            </div>
                          </>
                        )}
                      </div>
                      <div className="station-cluster">
                        <div className="field printer-position">
                          <Stepper id="vertical-position" label="Vertical position" unit="mm" min={-40} max={40} step={0.5} value={layout.vertical_offset_mm} disabled={busy} onChange={(n) => editLayout(setVertical(layout, n))} />
                          <small className="field-help">Moves the whole badge up (negative) or down (positive) on the paper.</small>
                          <div className="actions"><button type="button" disabled={busy || layout.vertical_offset_mm === 0} onClick={() => editLayout(setVertical(layout, 0))}>Reset to centre</button></div>
                        </div>
                      </div>
                    </div>
                  </li>
                </ul>
              </section>

              <section className={sectionClass("fields")}>
                <h2 className="setup-section-title">Badge fields</h2>
                <p className="setup-section-sub">Tick what to print and use the arrows to set the order from top to bottom.</p>
                <div className="station-card">
                  <CardHead icon={ICON.fields} title="Fields on the badge" chip={`${layout.elements.length} ticked`} />
                  <div className="station-card-body">
                    <label className="switch-row">
                      <span>Adjust bold, size and position</span>
                      <input type="checkbox" role="switch" className="switch" checked={adjust} onChange={(e) => setAdjust(e.target.checked)} />
                    </label>
                    <ul className="field-list">
                      {rows(layout).map((id) => {
                        const custom = layout.custom_fields[id];
                        const offset = offsetOf(layout, id);
                        const index = layout.elements.indexOf(id);
                        return (
                          <li className="field-row" key={id}>
                            <div className="field-row-head">
                              <label className="checkbox">
                                <input type="checkbox" checked={isOn(layout, id)} disabled={busy} onChange={(e) => editLayout(setOn(layout, id, e.target.checked))} />
                                {labelOf(id)}
                              </label>
                              <span className="field-move">
                                {adjust && isOn(layout, id) && (
                                  <label className="switch-inline">
                                    Bold
                                    <input type="checkbox" role="switch" className="switch" aria-label={`${labelOf(id)} bold`} checked={isBold(layout, id)} disabled={busy} onChange={(e) => editLayout(setBold(layout, id, e.target.checked))} />
                                  </label>
                                )}
                                <button type="button" aria-label={`Move ${labelOf(id)} up`} disabled={busy || index <= 0} onClick={() => editLayout(move(layout, id, -1))}>▲</button>
                                <button type="button" aria-label={`Move ${labelOf(id)} down`} disabled={busy || index < 0 || index === layout.elements.length - 1} onClick={() => editLayout(move(layout, id, 1))}>▼</button>
                              </span>
                            </div>
                            {custom && (
                              <div className="station-fields field-row-custom">
                                <div className="field">
                                  <label htmlFor={`custom-label-${id}`}>Label</label>
                                  <input id={`custom-label-${id}`} maxLength={60} value={custom.label} disabled={busy} onChange={(e) => editLayout(editCustom(layout, id, { label: e.target.value }))} />
                                </div>
                                <div className="field">
                                  <label htmlFor={`custom-key-${id}`}>Backend field key</label>
                                  <input id={`custom-key-${id}`} maxLength={60} value={custom.backend_key} disabled={busy} onChange={(e) => editLayout(editCustom(layout, id, { backend_key: e.target.value }))} />
                                </div>
                                <div className="field printer-remove">
                                  <button type="button" className="quiet-danger" disabled={busy} onClick={() => editLayout(removeCustom(layout, id))}>Remove field</button>
                                </div>
                              </div>
                            )}
                            {adjust && isOn(layout, id) && (
                              <div className="field-adjust">
                                <Stepper id={`size-${id}`} label="Size" unit="×" min={0.5} max={2} step={0.05} value={scaleOf(layout, id)} disabled={busy} onChange={(n) => editLayout(setScale(layout, id, n))} />
                                <Stepper id={`x-${id}`} label="Left / right" unit="mm" min={-10} max={10} step={0.5} value={offset.dx_mm} disabled={busy} onChange={(n) => editLayout(setOffset(layout, id, n, offset.dy_mm))} />
                                <Stepper id={`y-${id}`} label="Up / down" unit="mm" min={-10} max={10} step={0.5} value={offset.dy_mm} disabled={busy} onChange={(n) => editLayout(setOffset(layout, id, offset.dx_mm, n))} />
                              </div>
                            )}
                          </li>
                        );
                      })}
                    </ul>
                    <div className="actions">
                      <button type="button" disabled={busy || Object.keys(layout.custom_fields).length >= MAX_CUSTOM_FIELDS} onClick={() => editLayout(addCustom(layout, newCustomId(), "Custom field"))}>Add custom field</button>
                      <button type="button" disabled={busy} onClick={() => editLayout(resetAdjustments(layout))}>Reset adjustments</button>
                    </div>
                  </div>
                </div>
              </section>

              <section className={sectionClass("layouts")}>
                <h2 className="setup-section-title">Saved layouts</h2>
                <p className="setup-section-sub">Keep a layout for each kind of stock, and switch between them here.</p>
                <div className="station-card">
                  <CardHead icon={ICON.layers} title="Layouts on this PC" chip={`${Object.keys(settings.presets).length} saved`} />
                  <div className="station-card-body">
                    <div className="station-fields">
                      <div className="field">
                        <label htmlFor="layout-choice">Layout</label>
                        <select
                          id="layout-choice"
                          value={selectedPreset ?? ""}
                          disabled={busy}
                          onChange={(e) => {
                            const name = e.target.value;
                            change({ active_preset: name || null, ...(name ? { layout: settings.presets[name] } : {}) });
                          }}
                        >
                          <option value="">Current layout</option>
                          {Object.keys(settings.presets).map((name) => <option key={name} value={name}>{name}</option>)}
                        </select>
                      </div>
                      <div className="field">
                        <label htmlFor="layout-name">Save the current layout as</label>
                        <div className="inline-test">
                          <input id="layout-name" maxLength={40} placeholder="e.g. Badge card" value={presetName} disabled={busy} onChange={(e) => setPresetName(e.target.value)} />
                          <button
                            type="button"
                            disabled={busy || !presetName.trim() || Object.keys(settings.presets).length >= 20 || presetName.trim() in settings.presets}
                            onClick={() => {
                              const name = presetName.trim();
                              change({ presets: { ...settings.presets, [name]: layout }, active_preset: name });
                              setPresetName("");
                            }}
                          >
                            Save as new
                          </button>
                        </div>
                      </div>
                    </div>
                    <div className="actions">
                      <button type="button" disabled={busy || !selectedPreset} onClick={() => { if (selectedPreset) change({ presets: { ...settings.presets, [selectedPreset]: layout } }); }}>Update selected layout</button>
                      <button type="button" className="quiet-danger" disabled={busy || !selectedPreset} onClick={() => { const presets = { ...settings.presets }; if (selectedPreset) delete presets[selectedPreset]; change({ presets, active_preset: null }); }}>Delete selected layout</button>
                    </div>
                    <small className="field-help">Layout changes are kept on this PC when you press Save printer settings.</small>
                  </div>
                </div>
              </section>

              <section className={sectionClass("manual")}>
                <h2 className="setup-section-title">Manual print</h2>
                <p className="setup-section-sub">Print one badge from details you type. Use it to check the printer is connected and the layout is right, or to print for a guest who is not in EventzFlow.</p>
                <p className="field-help printer-follows">The guest details below are the fields ticked in Badge fields, in the same order. Change which fields print there.</p>
                <div className="station-card">
                  <CardHead icon={ICON.printer} title="Print to" chip={settings.printer || view.printers.default || "No printer"} />
                  <div className="station-card-body">
                    <p className={printerStatus.ok ? "note" : "failure"} role="status">{printerStatus.text}</p>
                    <small className="field-help">Works with either printing method and never changes a desk. It uses the printer, paper and layout set in this page, and saves any changes first.</small>
                  </div>
                </div>
                <div className="station-card">
                  <CardHead icon={ICON.layers} title="Badge types" chip={`${settings.badge_types.length} added`} />
                  <div className="station-card-body">
                    <p className="printer-explain">Ticket types you add here are offered every time in the guest's Ticket type list, next to the event's own, so you can pick one for a badge.</p>
                    {settings.badge_types.length > 0 && (
                      <ul className="type-chips">
                        {settings.badge_types.map((name) => (
                          <li key={name}>
                            <span>{name}</span>
                            <button type="button" aria-label={`Remove ${name}`} disabled={busy} onClick={() => change({ badge_types: settings.badge_types.filter((t) => t !== name) })}>×</button>
                          </li>
                        ))}
                      </ul>
                    )}
                    <div className="inline-test">
                      <input aria-label="New ticket type" maxLength={40} placeholder="e.g. Sponsor" value={newType} disabled={busy} onChange={(e) => setNewType(e.target.value)} />
                      <button
                        type="button"
                        disabled={busy || !newType.trim() || settings.badge_types.length >= 30 || settings.badge_types.some((t) => t.toLowerCase() === newType.trim().toLowerCase())}
                        onClick={() => { change({ badge_types: [...settings.badge_types, newType.trim()] }); setNewType(""); }}
                      >
                        Add type
                      </button>
                    </div>
                    <small className="field-help">Types are kept on this PC when you press Save printer settings or print a badge.</small>
                  </div>
                </div>
                <div className="station-card printer-guest-card">
                  <CardHead icon={ICON.fields} title="Guest on the badge" />
                  <div className="station-card-body">
                    {guestFields}
                    <div className="actions">
                      <button type="button" className="primary" disabled={busy || !view.printers.supported} onClick={() => void run(async () => { await save(); const message = await badgePrint(ticket); if (mounted.current) setNotice(message); })}>
                        {busy ? "Working…" : "Print badge"}
                      </button>
                      <button type="button" disabled={busy} onClick={() => setTicket(sample)}>Use the sample guest</button>
                    </div>
                  </div>
                </div>
              </section>

              <section className={sectionClass("import")}>
                <h2 className="setup-section-title">Import from event-printing</h2>
                <p className="setup-section-sub">Bring over the badge designs you already made in the event-printing app on this PC.</p>
                <div className="station-card">
                  <CardHead icon={ICON.import} title="event-printing layouts" />
                  <div className="station-card-body">
                    <p className="printer-explain">
                      Copies the layout, the saved layouts and the thin-stroke setting from event-printing's saved
                      settings on this PC. Your printer choice and the printing method are kept. Its address, event
                      name and API key are never copied. The import replaces the layouts here and saves straight away.
                    </p>
                    {!importConfirm ? (
                      <div className="actions">
                        <button type="button" disabled={busy} onClick={() => setImportConfirm(true)}>Import from event-printing…</button>
                      </div>
                    ) : (
                      <div className="note">
                        <p>Replace the current layout and saved layouts with event-printing's?</p>
                        <div className="actions">
                          <button type="button" className="primary" disabled={busy} onClick={() => void run(async () => { accept(await badgeImport()); setImportConfirm(false); setNotice("Layouts imported. Check the preview before printing."); })}>Replace layouts</button>
                          <button type="button" disabled={busy} onClick={() => setImportConfirm(false)}>Cancel</button>
                        </div>
                      </div>
                    )}
                  </div>
                </div>
              </section>
            </div>

            <aside className="station-card printer-side" aria-label="Live preview">
              <CardHead icon={ICON.eye} title="Live preview" chip={`${layout.paper.width_mm} × ${layout.paper.height_mm} mm`} />
              <div className="station-card-body">
                {previewError && <p className="failure" role="alert">{previewError}</p>}
                <div className="printer-paper">
                  {image ? <img src={image} alt="Sample badge preview" /> : <p className="hint">Drawing preview…</p>}
                </div>
                <small className="field-help">Sample guest only. The preview never prints.</small>
                {section !== "manual" && (
                  <details className="printer-sample">
                    <summary>Sample guest</summary>
                    {guestFields}
                  </details>
                )}
              </div>
            </aside>
          </div>
        </div>

        <div className="savebar">
          <button type="button" className="primary" disabled={busy || !dirty} onClick={() => void run(async () => { await save(); setNotice("Printer settings saved."); })}>
            {busy ? "Working…" : "Save printer settings"}
          </button>
          <button type="button" disabled={busy || !view.printers.supported} onClick={() => void run(async () => { await save(); const message = await badgeTestPrint(); if (mounted.current) setNotice(message); })}>
            Test print
          </button>
          {dirty ? <span className="unsaved">Unsaved changes</span> : <span className="savebar-note">Everything is saved</span>}
        </div>
      </div>
    </div>
  );
}
