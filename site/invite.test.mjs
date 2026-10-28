// The landing page's logic, under node's test runner: `node --test site/`.
import assert from "node:assert/strict";
import test from "node:test";

import { micLink } from "./invite.js";

// mic-core's fixture: room 16 bytes of 1, secret 32 bytes of 2
// (crates/mic-core/src/invite.rs, `the_landing_pages_fixture`).
const ROOM = "AQEBAQEBAQEBAQEBAQEBAQ";
const SECRET = "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI";

test("a landing page's fragment opens mic:// with the same invite", () => {
    assert.deepEqual(micLink(`#1/${ROOM}/${SECRET}`), {
        link: `mic://join/1/${ROOM}#${SECRET}`,
    });
});

test("a room's name goes along", () => {
    // "Standup", as mic-core writes it.
    assert.deepEqual(micLink(`#1/${ROOM}/${SECRET}/U3RhbmR1cA`), {
        link: `mic://join/1/${ROOM}#${SECRET}/U3RhbmR1cA`,
    });
});

test("what isn't an invite says so", () => {
    assert.ok("error" in micLink(""));
    assert.ok("error" in micLink(`#2/${ROOM}/${SECRET}`));
    assert.ok("error" in micLink(`#1/${ROOM}/short`));
    assert.ok("error" in micLink(`#1/${ROOM.slice(1)}!/${SECRET}`));
    assert.ok("error" in micLink(`#1/${ROOM}/${SECRET}/not a name`));
});
