import assert from "node:assert/strict";
import {
  addCustom, editCustom, isBold, MAX_CUSTOM_FIELDS, move, newCustomId, paperPresetId,
  removeCustom, resetAdjustments, rows, setOn, setScale,
} from "../src/badge-layout.ts";

const base = {
  paper: { width_mm: 100, height_mm: 80 },
  elements: ["name", "role", "company", "qr"],
  custom_fields: {}, element_scales: {}, element_bolds: {}, element_offsets: {},
  vertical_offset_mm: 0,
};

// Ticking adds to the end of the badge; unticking removes; ticking twice is harmless.
assert.deepEqual(setOn(base, "title", true).elements, ["name", "role", "company", "qr", "title"]);
assert.equal(setOn(base, "name", true), base);
assert.deepEqual(setOn(base, "role", false).elements, ["name", "company", "qr"]);

// Ticked fields come first in badge order, then the rest.
assert.deepEqual(rows(base).slice(0, 4), ["name", "role", "company", "qr"]);
assert.equal(rows(base).length, 8);

// Moving swaps neighbours and stops at the ends.
assert.deepEqual(move(base, "role", -1).elements, ["role", "name", "company", "qr"]);
assert.equal(move(base, "name", -1), base);
assert.equal(move(base, "qr", 1), base);
assert.equal(move(base, "title", 1), base, "an unticked field has no place to move");

// Paper presets are recognised by size.
assert.equal(paperPresetId(base), "sticker");
assert.equal(paperPresetId({ ...base, paper: { width_mm: 104, height_mm: 155 } }), "card");
assert.equal(paperPresetId({ ...base, paper: { width_mm: 90, height_mm: 55 } }), "custom");

// Weight defaults: name, role and table are bold; the rest are not.
assert.equal(isBold(base, "name"), true);
assert.equal(isBold(base, "company"), false);
assert.equal(isBold({ ...base, element_bolds: { company: true } }, "company"), true);

// Custom fields: added ticked, capped at six, edited, removed with their settings.
let layout = base;
for (let i = 0; i < MAX_CUSTOM_FIELDS + 2; i++) layout = addCustom(layout, `custom_00000${i}`, `Field ${i}`);
assert.equal(Object.keys(layout.custom_fields).length, MAX_CUSTOM_FIELDS);
assert.equal(layout.elements.filter((e) => e.startsWith("custom_")).length, MAX_CUSTOM_FIELDS);
layout = editCustom(layout, "custom_000000", { backend_key: "sponsor" });
assert.equal(layout.custom_fields.custom_000000.backend_key, "sponsor");
assert.equal(layout.custom_fields.custom_000000.label, "Field 0");
layout = setScale(layout, "custom_000000", 1.5);
layout = removeCustom(layout, "custom_000000");
assert.equal("custom_000000" in layout.custom_fields, false);
assert.equal(layout.elements.includes("custom_000000"), false);
assert.equal("custom_000000" in layout.element_scales, false);

// Reset clears size, weight and position only.
const adjusted = { ...base, element_scales: { name: 1.4 }, vertical_offset_mm: 5 };
const reset = resetAdjustments(adjusted);
assert.deepEqual([reset.element_scales, reset.vertical_offset_mm, reset.elements], [{}, 0, base.elements]);

// Ids look like the ones Rust accepts: custom_ plus six hex characters.
assert.match(newCustomId(), /^custom_[0-9a-f]{6}$/);
console.log("Badge layout edit checks passed.");
