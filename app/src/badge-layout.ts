// Pure edits of a badge layout. The Printer tab calls these; Rust cleans the
// result when it is saved or drawn, so nothing here has to be strict.
import type { BadgeLayout } from "./api";

export const BUILT_IN = [
  "name",
  "role",
  "ticket_role",
  "company",
  "title",
  "country",
  "table_no",
  "qr",
] as const;

export const FIELD_LABEL: Record<string, string> = {
  name: "Name",
  role: "Ticket type",
  ticket_role: "Role",
  company: "Company",
  title: "Job title",
  country: "Country",
  table_no: "Table number",
  qr: "QR code",
};

export const PAPER_PRESETS = [
  { id: "sticker", label: "Sticker — 100 × 80 mm", width_mm: 100, height_mm: 80 },
  { id: "card", label: "Badge card — 104 × 155 mm", width_mm: 104, height_mm: 155 },
] as const;

export const MAX_CUSTOM_FIELDS = 6;

const BOLD_BY_DEFAULT = new Set(["name", "role", "table_no"]);

function omit<T>(map: Record<string, T>, key: string): Record<string, T> {
  const copy = { ...map };
  delete copy[key];
  return copy;
}

export function paperPresetId(layout: BadgeLayout): "sticker" | "card" | "custom" {
  const { width_mm, height_mm } = layout.paper;
  const found = PAPER_PRESETS.find((p) => p.width_mm === width_mm && p.height_mm === height_mm);
  return found ? found.id : "custom";
}

export const isOn = (layout: BadgeLayout, id: string) => layout.elements.includes(id);

export const isBold = (layout: BadgeLayout, id: string) =>
  layout.element_bolds[id] ?? BOLD_BY_DEFAULT.has(id);

export const scaleOf = (layout: BadgeLayout, id: string) => layout.element_scales[id] ?? 1;

export const offsetOf = (layout: BadgeLayout, id: string) =>
  layout.element_offsets[id] ?? { dx_mm: 0, dy_mm: 0 };

/** Every field that could be on the badge: ticked ones first, in badge order. */
export function rows(layout: BadgeLayout): string[] {
  const all = [...BUILT_IN, ...Object.keys(layout.custom_fields)];
  return [...layout.elements, ...all.filter((id) => !layout.elements.includes(id))];
}

export function setOn(layout: BadgeLayout, id: string, on: boolean): BadgeLayout {
  if (on) {
    return isOn(layout, id) ? layout : { ...layout, elements: [...layout.elements, id] };
  }
  return { ...layout, elements: layout.elements.filter((e) => e !== id) };
}

export function move(layout: BadgeLayout, id: string, step: -1 | 1): BadgeLayout {
  const from = layout.elements.indexOf(id);
  const to = from + step;
  if (from < 0 || to < 0 || to >= layout.elements.length) return layout;
  const elements = [...layout.elements];
  [elements[from], elements[to]] = [elements[to], elements[from]];
  return { ...layout, elements };
}

export const setScale = (layout: BadgeLayout, id: string, scale: number): BadgeLayout => ({
  ...layout,
  element_scales: { ...layout.element_scales, [id]: scale },
});

export const setBold = (layout: BadgeLayout, id: string, bold: boolean): BadgeLayout => ({
  ...layout,
  element_bolds: { ...layout.element_bolds, [id]: bold },
});

export const setOffset = (layout: BadgeLayout, id: string, dx: number, dy: number): BadgeLayout => ({
  ...layout,
  element_offsets: { ...layout.element_offsets, [id]: { dx_mm: dx, dy_mm: dy } },
});

export const setVertical = (layout: BadgeLayout, mm: number): BadgeLayout => ({
  ...layout,
  vertical_offset_mm: mm,
});

export const setPaper = (layout: BadgeLayout, width_mm: number, height_mm: number): BadgeLayout => ({
  ...layout,
  paper: { width_mm, height_mm },
});

/** Back to automatic size, weight and position for every field. */
export const resetAdjustments = (layout: BadgeLayout): BadgeLayout => ({
  ...layout,
  element_scales: {},
  element_bolds: {},
  element_offsets: {},
  vertical_offset_mm: 0,
});

export function newCustomId(): string {
  return `custom_${Math.random().toString(16).slice(2, 8).padEnd(6, "0")}`;
}

/** A new custom field is added already ticked. Past the limit nothing changes. */
export function addCustom(layout: BadgeLayout, id: string, label: string): BadgeLayout {
  if (Object.keys(layout.custom_fields).length >= MAX_CUSTOM_FIELDS || id in layout.custom_fields) {
    return layout;
  }
  return {
    ...layout,
    custom_fields: { ...layout.custom_fields, [id]: { label, backend_key: "" } },
    elements: [...layout.elements, id],
  };
}

export function editCustom(
  layout: BadgeLayout,
  id: string,
  change: Partial<{ label: string; backend_key: string }>,
): BadgeLayout {
  const field = layout.custom_fields[id];
  if (!field) return layout;
  return { ...layout, custom_fields: { ...layout.custom_fields, [id]: { ...field, ...change } } };
}

export function removeCustom(layout: BadgeLayout, id: string): BadgeLayout {
  return {
    ...layout,
    custom_fields: omit(layout.custom_fields, id),
    elements: layout.elements.filter((e) => e !== id),
    element_scales: omit(layout.element_scales, id),
    element_bolds: omit(layout.element_bolds, id),
    element_offsets: omit(layout.element_offsets, id),
  };
}
