import {test} from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import {JcodeClient, HarnessError, type RuntimeControlRequest} from "../dist/index.js";
import {matchesRuntimeResponse} from "../dist/runtime-control.js";
import {startMockHarness} from "./mock-harness.ts";

const cases = JSON.parse(fs.readFileSync(new URL("../../../crates/jcode-workspace-types/src/runtime_correlation.json",import.meta.url),"utf8"));
test("shared runtime correlation and shape matrix matches Rust",()=> {
  assert(cases.length > 0);
  for (const item of cases) assert.equal(matchesRuntimeResponse(item.request,item.response),item.accepted,item.name);
});

test("runtime controls require bridge and exact native support before mutation", async()=> {
  for (const version of [undefined,null,0,2]) {
    let controls=0;
    const server=await startMockHarness({capabilities:version === undefined ? [] : ["runtime_lifecycle_v1"],onRequest(request,send) {
      if (request.req === "runtime_probe") send({v:1,reply_to:request.id,ev:"runtime_capabilities",version});
      else controls++;
    }});
    const client=await JcodeClient.connect({socketPath:server.socketPath,ensureRuntime:false});
    try {
      await assert.rejects(()=>client.runtimeControl({action:"status"}), (error:unknown)=>error instanceof HarnessError && error.code === "unsupported_capability");
      assert.equal(controls,0);
    } finally {client.close(); await server.close();}
  }
});

test("runtime socket client validates every control and creates no Session",async()=> {
  for (const item of cases) {
    const server=await startMockHarness({capabilities:["runtime_lifecycle_v1"],onRequest(request,send) {
      if (request.req === "runtime_probe") {send({v:1,reply_to:request.id,ev:"runtime_capabilities",version:1});return;}
      assert.equal(request.req,"runtime_control");
      assert.deepEqual(request.request,item.request);
      send({v:1,reply_to:request.id,ev:"runtime_control",response:item.response});
    }});
    const client=await JcodeClient.connect({socketPath:server.socketPath,ensureRuntime:false});
    try {
      if (item.accepted) assert.deepEqual(await client.runtimeControl(item.request),item.response,item.name);
      else await assert.rejects(()=>client.runtimeControl(item.request), (error:unknown)=>error instanceof HarnessError && error.code === "unexpected_reply",item.name);
    } finally {client.close();await server.close();}
  }
});

test("runtime caller settings freeze before capability wait and unsafe revisions never dispatch",async()=> {
  const item=cases.find((item:{name:string})=>item.name === "review valid");
  const input:RuntimeControlRequest=structuredClone(item.request);
  const expected=structuredClone(input);
  const server=await startMockHarness({capabilities:["runtime_lifecycle_v1"],onRequest(request,send) {
    if (request.req === "runtime_probe") {
      if (input.action === "review") input.options.independent="stop";
      send({v:1,reply_to:request.id,ev:"runtime_capabilities",version:1});return;
    }
    assert.equal(request.req,"runtime_control");assert.deepEqual(request.request,expected);
    send({v:1,reply_to:request.id,ev:"runtime_control",response:item.response});
  }});
  const client=await JcodeClient.connect({socketPath:server.socketPath,ensureRuntime:false});
  try {
    assert.equal((await client.runtimeControl(input)).kind,"review");
    await assert.rejects(()=>client.runtimeControl({action:"force",operation:"12a99e11-e967-4a1e-a47c-000000000002",expected_revision:Number.MAX_SAFE_INTEGER+1}), (error:unknown)=>error instanceof HarnessError && error.code === "invalid_request");
    const unsafeReply=structuredClone(item.response);unsafeReply.value.revision=Number.MAX_SAFE_INTEGER+1;
    assert.equal(matchesRuntimeResponse(expected,unsafeReply),false);
  } finally {client.close();await server.close();}
});
