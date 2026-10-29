import assert from "node:assert/strict";
import { test } from "node:test";
import { powerHint } from "./power.ts";

// `jobs` are what runs now; `wanted` is what each level alone would allow (defaults to the same).
const info = (jobs: number[], free: number | null = 23000, wanted: number[] = jobs) => ({
  cores: 32,
  free_mb: free,
  choices: (["light", "balanced", "full"] as const).map((name, i) => ({
    name,
    jobs: jobs[i],
    wanted_jobs: wanted[i],
    threads_per_job: jobs[i] === 1 ? 4 : 2,
  })),
});

test("says how many episodes are made at once at the chosen level", () => {
  assert.match(powerHint(info([4, 8, 16]), "balanced"), /8 集/);
  assert.match(powerHint(info([4, 8, 16]), "light"), /4 集/);
  assert.match(powerHint(info([4, 8, 16]), "full"), /16 集/);
});

test("a single job is described as one episode, not as a number of parallel ones", () => {
  const hint = powerHint(info([1, 1, 1]), "full");
  assert.match(hint, /一次生成 1 集/);
  assert.doesNotMatch(hint, /同时生成/);
});

test("warns when memory, not the level, is what limits the work", () => {
  // the level would allow 16, memory only allows 2
  const hint = powerHint(info([2, 2, 2], 4000, [4, 8, 16]), "full");
  assert.match(hint, /内存/);
  assert.match(hint, /2 集/);
  assert.match(hint, /16/);
});

test("no memory warning when the level is the limit", () => {
  assert.doesNotMatch(powerHint(info([4, 8, 16]), "balanced"), /受内存限制/);
});

test("no memory warning for a level that memory does not restrict, even if another one is", () => {
  const mixed = info([2, 2, 2], 4000, [2, 8, 16]);
  assert.doesNotMatch(powerHint(mixed, "light"), /受内存限制/);
  assert.match(powerHint(mixed, "balanced"), /受内存限制/);
});

test("shows free memory in GB when it is known", () => {
  assert.match(powerHint(info([4, 8, 16], 23552), "light"), /23\.0 GB|23 GB/);
});

test("copes with unknown memory and with an unknown level name", () => {
  assert.doesNotMatch(powerHint(info([4, 8, 16], null), "light"), /NaN|undefined|null/);
  assert.equal(powerHint(info([4, 8, 16]), "turbo" as never), "");
});

test("warns that the higher levels use more of the computer", () => {
  assert.match(powerHint(info([4, 8, 16]), "full"), /占用|忙|变慢/);
});
