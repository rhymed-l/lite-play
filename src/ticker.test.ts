import assert from "node:assert/strict";
import { test } from "node:test";
import { colours, scrollOffset } from "./ticker.ts";

// scrollOffset(textWidth, boxWidth, cueStart, cueEnd, position) -> px to shift the text left.
// The text must be fully readable by the time the cue ends, and never move when it fits.

test("text that fits in the box does not move at all", () => {
  assert.equal(scrollOffset(400, 800, 10, 14, 10), 0);
  assert.equal(scrollOffset(400, 800, 10, 14, 12), 0);
  assert.equal(scrollOffset(800, 800, 10, 14, 13), 0);
});

test("long text starts at its beginning and ends at its end", () => {
  const overflow = 1200 - 800;
  assert.equal(scrollOffset(1200, 800, 10, 20, 10), 0);
  assert.equal(scrollOffset(1200, 800, 10, 20, 20), overflow);
  assert.equal(scrollOffset(1200, 800, 10, 20, 25), overflow, "stays at the end after the cue is over");
});

test("the start of a long line is held still for a moment so it can be read", () => {
  // first 15 % of the cue: no movement
  assert.equal(scrollOffset(1200, 800, 10, 20, 11), 0);
  assert.equal(scrollOffset(1200, 800, 10, 20, 11.5), 0);
});

test("the end of a long line is reached before the cue ends and held", () => {
  // the last 10 % of the cue: already at the end
  assert.equal(scrollOffset(1200, 800, 10, 20, 19), 400);
  assert.equal(scrollOffset(1200, 800, 10, 20, 19.5), 400);
});

test("the middle moves steadily and never goes backwards", () => {
  let previous = -1;
  for (let t = 10; t <= 20; t += 0.25) {
    const offset = scrollOffset(1200, 800, 10, 20, t);
    assert.ok(offset >= previous, `went backwards at ${t}s: ${offset} < ${previous}`);
    assert.ok(offset >= 0 && offset <= 400, `out of range at ${t}s: ${offset}`);
    previous = offset;
  }
});

test("a very short cue with long text still reaches the end", () => {
  assert.equal(scrollOffset(2000, 800, 5, 5.5, 5.5), 1200);
  assert.equal(scrollOffset(2000, 800, 5, 5.5, 5.0), 0);
});

test("a position before the cue shows its beginning", () => {
  assert.equal(scrollOffset(1200, 800, 10, 20, 3), 0);
});

test("a zero or negative length cue shows its end rather than dividing by zero", () => {
  assert.equal(scrollOffset(1200, 800, 10, 10, 10), 400);
  assert.equal(scrollOffset(1200, 800, 10, 9, 10), 400);
});

test("nonsense widths never produce NaN", () => {
  assert.equal(scrollOffset(NaN, 800, 10, 20, 15), 0);
  assert.equal(scrollOffset(1200, 0, 10, 20, 15), 0);
  assert.equal(scrollOffset(1200, 800, 10, 20, NaN), 0);
});

test("built-in colour schemes", () => {
  const base = { custom_fg: "#112233", custom_bg: "#445566" };
  assert.deepEqual(colours({ ...base, scheme: "dark" }), { fg: "#ffffff" });
  assert.deepEqual(colours({ ...base, scheme: "light" }), { fg: "#111111" });
  assert.deepEqual(colours({ ...base, scheme: "yellow" }), { fg: "#ffe14d" });
});

test("the custom scheme uses the user's text colour", () => {
  assert.deepEqual(colours({ scheme: "custom", custom_fg: "#abcdef", custom_bg: "#000000" }), { fg: "#abcdef" });
});

test("an unknown scheme falls back to the dark one", () => {
  assert.deepEqual(colours({ scheme: "nope", custom_fg: "#112233", custom_bg: "#445566" }), { fg: "#ffffff" });
});
