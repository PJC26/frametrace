#!/usr/bin/env node
// Parity check: the viewer's TIME_PATTERNS / parseTimeFromName
// (assets/viewer/30-records.js) must agree with the shared fixture
// tests/fixtures/rectime-cases.json, which src/rectime.rs's unit tests also
// read. A pattern change on either side without the other fails here or in
// `cargo test`.
//
// The JS source is extracted verbatim and evaluated in a sandbox whose
// `Date(y, m, d, hh, mm, ss)` counts the wall-clock fields as UTC, so the
// result is the same naive seconds value Rust computes, independent of the
// machine's time zone (day overflow still rolls over like the real Date).
//
// Usage: node scripts/check-rectime-fixture.mjs
//        node scripts/check-rectime-fixture.mjs --print "<name>" ...   (show values)

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";
import vm from "node:vm";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const source = readFileSync(path.join(root, "assets/viewer/30-records.js"), "utf8");

function extract(startMarker, endMarker) {
  const start = source.indexOf(startMarker);
  if (start < 0) throw new Error(`30-records.js: '${startMarker}' not found`);
  const end = source.indexOf(endMarker, start);
  if (end < 0) throw new Error(`30-records.js: end of '${startMarker}' not found`);
  return source.slice(start, end + endMarker.length);
}

const patternsSrc = extract("const TIME_PATTERNS = [", "\n];");
const parseSrc = extract("function parseTimeFromName(name) {", "\n}");

class NaiveDate {
  constructor(...args) {
    this.value = args.length >= 3 ? Date.UTC(...args) : new Date(...args).getTime();
  }
  getTime() {
    return this.value;
  }
}

const sandbox = { Date: NaiveDate, String, Number };
vm.createContext(sandbox);
vm.runInContext(`${patternsSrc}\n${parseSrc}\nthis.parseTimeFromName = parseTimeFromName;`, sandbox);
const parse = sandbox.parseTimeFromName;

function jsView(input) {
  const got = parse(input);
  if (!got) return null;
  return { secs_naive: got.ts, date: got.date, has_time: got.source === "name" };
}

const args = process.argv.slice(2);
if (args[0] === "--print") {
  for (const input of args.slice(1)) console.log(JSON.stringify({ input, expect: jsView(input) }));
  process.exit(0);
}

const fixture = JSON.parse(readFileSync(path.join(root, "tests/fixtures/rectime-cases.json"), "utf8"));
let failures = 0;
for (const { input, expect } of fixture.cases) {
  const got = jsView(input);
  const want = expect && { secs_naive: expect.secs_naive, date: expect.date, has_time: expect.has_time };
  if (JSON.stringify(got) !== JSON.stringify(want)) {
    failures += 1;
    console.error(`MISMATCH ${JSON.stringify(input)}: js=${JSON.stringify(got)} fixture=${JSON.stringify(want)}`);
  }
}
if (failures) {
  console.error(`check-rectime-fixture: ${failures} of ${fixture.cases.length} case(s) disagree`);
  process.exit(1);
}
console.log(`check-rectime-fixture: ${fixture.cases.length} cases agree with 30-records.js TIME_PATTERNS`);
