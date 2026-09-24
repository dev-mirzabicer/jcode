import {test} from "node:test";
import assert from "node:assert/strict";
import {JcodeClient, HarnessError, type CloseoutRequest} from "../dist/index.js";
import {startMockHarness} from "./mock-harness.ts";
import fs from "node:fs";
import {matchesCloseoutReply} from "../dist/closeout.js";

test("shared Rust/TypeScript closeout correlation matrix", () => {
  const cases = JSON.parse(fs.readFileSync(new URL("../../../crates/jcode-workspace-types/src/closeout/correlation.json", import.meta.url), "utf8"));
  assert(cases.length > 0);
  for (const item of cases) assert.equal(matchesCloseoutReply(item.request, item.reply), item.accepted, item.name);
});

const operation = "12a99e11-e967-4a1e-a47c-000000000007";
const command: CloseoutRequest = {action:"inventory", operation, digest:"fixture", after:0, limit:10};

test("closeout rejects missing bridge/native support before control submission", async () => {
  for (const mode of ["old", "none", "newer"]) {
    let controls = 0;
    const server = await startMockHarness({capabilities: mode === "old" ? [] : ["checkout_closeout_v1"], onRequest(request, send) {
      if (request.req === "closeout_probe") send({v:1, reply_to:request.id, ev:"closeout_capabilities", version:mode === "none" ? null : 2});
      else controls++;
    }});
    const client = await JcodeClient.connect({socketPath:server.socketPath, ensureRuntime:false});
    try {
      await assert.rejects(() => client.closeout(command), (error: unknown) => error instanceof HarnessError && error.code === "unsupported_capability");
      assert.equal(controls, 0);
    } finally {client.close(); await server.close();}
  }
});

test("closeout retains typed refusal and rejects foreign target/digest without a Session", async () => {
  for (const mode of ["correct", "foreign", "rejected", "malformed"]) {
    const server = await startMockHarness({capabilities:["checkout_closeout_v1"], onRequest(request, send) {
      if (request.req === "closeout_probe") {send({v:1, reply_to:request.id, ev:"closeout_capabilities", version:1}); return;}
      assert.equal(request.req, "closeout");
      assert.deepEqual(request.request, command);
      const reply = mode === "malformed" ? {status:"state", response:{kind:"inventory",value:null}} : mode === "rejected" ? {status:"rejected", issue:{code:"preservation_incomplete", detail:"synthetic"}} : {
        status:"state", response:{kind:"inventory", value:{operation, digest:mode === "foreign" ? "other" : "fixture", total:0, entries:[], next:null}},
      };
      send({v:1, reply_to:request.id, ev:"closeout", reply});
    }});
    const client = await JcodeClient.connect({socketPath:server.socketPath, ensureRuntime:false});
    try {
      if (mode === "foreign" || mode === "malformed") await assert.rejects(() => client.closeout(command), (error: unknown) => error instanceof HarnessError && error.code === "unexpected_reply");
      else assert.equal((await client.closeout(command)).status, mode === "rejected" ? "rejected" : "state");
    } finally {client.close(); await server.close();}
  }
});

test("closeout freezes caller intent before capability wait and compares object fields structurally", async () => {
  const input: CloseoutRequest = {action:"execute", request:"12a99e11-e967-4a1e-a47c-000000000008", spec:{operation, expected_revision:7, action:{action:"finish"}}};
  const expected = structuredClone(input);
  const server = await startMockHarness({capabilities:["checkout_closeout_v1"], onRequest(request, send) {
    if (request.req === "closeout_probe") {
      input.spec.expected_revision = 99;
      send({v:1, reply_to:request.id, ev:"closeout_capabilities", version:1}); return;
    }
    assert.equal(request.req, "closeout");
    assert.deepEqual(request.request, expected);
    send({v:1, reply_to:request.id, ev:"closeout", reply:{status:"state", response:{kind:"action", value:{
      request:expected.request, spec:{action:{action:"finish"}, expected_revision:7, operation},
      initiated_by:"fixture", run_id:"run-fixture", result:null, issue:null,
    }}}});
  }});
  const client = await JcodeClient.connect({socketPath:server.socketPath, ensureRuntime:false});
  try {assert.equal((await client.closeout(input)).status, "state");}
  finally {client.close(); await server.close();}
});
