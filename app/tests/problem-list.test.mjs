import assert from "node:assert/strict";
import { problemPage } from "../src/problem-list.ts";

const items = Array.from({ length: 2001 }, (_, id) => ({
  id, station_id: "gate-a", station_name: "Hall A entrance",
  name: id === 0 ? null : `Guest ${id}`, message: "Server reply needs review",
  captured_at: "2026-10-01T08:00:00Z",
}));
assert.equal(problemPage(items, "", 0).rows.length, 25);
assert.equal(problemPage(items, "", 80).rows.length, 1);
assert.equal(problemPage(items, "", 80).pages, 81);
assert.equal(problemPage(items, " GUEST 2000 ", 0).rows[0].id, 2000);
assert.equal(problemPage(items, "hall a", 0).total, 2001);
assert.equal(problemPage(items, "reply", 0).total, 2001);
assert.equal(problemPage(items, "unknown sticker", 0).total, 1);
assert.equal(problemPage(items, "missing", 4).page, 0);
assert.equal(problemPage(items, "missing", 4).rows.length, 0);
assert.equal(problemPage(items.slice(0, 25), "", 80).page, 0);
assert.equal(items.length, 2001);
console.log("Problem search and pagination checks passed (2,001 rows).");
