// Real SDK/bridge/native-daemon closeout journey. Owned fixture only.
import {JcodeClient} from "../sdk/typescript/dist/index.js";
import fs from "node:fs";
import path from "node:path";
import assert from "node:assert/strict";
import {createHash, randomUUID} from "node:crypto";
const root = process.env.JCODE_WP07_NATIVE_ROOT;
assert(root && process.env.JCODE_TEST_STATE_ROOT && fs.existsSync(path.join(root, "wp07-owner.json")));
const fixture = JSON.parse(fs.readFileSync(path.join(root, "fixture.json"), "utf8"));
let client;
const actions = [];
const intents = [];
const evidence = {actions, intents, disconnected_while_running:false, cancelled:false};
const terminal = new Set(["completed", "cancelled", "failed", "interrupted"]);
const connect = async () => JcodeClient.connect({socketPath:path.join(root,"api.sock"), ensureRuntime:false, requestTimeoutMs:120000});
const save = () => fs.writeFileSync(path.join(root, "sdk-progress.json"), JSON.stringify(evidence,null,2));
async function call(request) {
  const reply = await client.closeout(request);
  assert.equal(reply.status, "state", JSON.stringify(reply));
  return reply.response;
}
async function inspect(operation) { const reply=await call({action:"inspect", operation}); assert.equal(reply.kind,"record"); return reply.value; }
async function submit(record, action) {
  const request=randomUUID();
  const spec={operation:record.operation, expected_revision:record.revision, action};
  intents.push({request,spec}); save();
  const reply=await call({action:"execute", request, spec});
  assert.equal(reply.kind,"action"); actions.push(reply.value); save(); return reply.value;
}
async function wait(action, expected="completed") {
  const deadline=Date.now()+180000;
  while (Date.now()<deadline) {
    const reply=await call({action:"execution", request:action.request, control:{action:"inspect", run_id:action.run_id}});
    assert.equal(reply.kind,"execution"); assert.equal(reply.value.kind,"status");
    const run=reply.value.run;
    if (terminal.has(run.state)) {
      assert.equal(run.state,expected,JSON.stringify(run)); assert.equal(run.complete,true);
      const result=await call({action:"inspect_action",request:action.request});
      assert.equal(result.kind,"action");
      if(expected==="completed") assert.equal(result.value.issue,null,JSON.stringify(result));
      return result.value;
    }
    await new Promise(resolve=>setTimeout(resolve,100));
  }
  throw new Error(`Closeout execution deadline: ${action.run_id}`);
}
async function perform(record, action) {return (await wait(await submit(record,action))).result;}
try {
  client=await connect();
  const begun=await call({action:"begin",request:randomUUID(),expected_revision:fixture.revision,spec:{location:fixture.location,expected_generation:1,preservation_directory:null,conditional_no_loss:false,full_archive:true}});
  assert.equal(begun.kind,"record"); let record=begun.value;
  const refresh=await submit(record,{action:"refresh"});
  const running=await call({action:"execution",request:refresh.request,control:{action:"inspect",run_id:refresh.run_id}});
  assert.equal(running.value.run.state,"running","fixture must detach from actual running work");
  client.close(); client=null;
  // No API or native clients remain attached during this interval.
  await new Promise(resolve=>setTimeout(resolve,350));
  client=await connect();
  record=(await wait(refresh)).result.value;
  evidence.disconnected_while_running=true; save();
  const replay=await call({action:"execute",request:refresh.request,spec:refresh.spec});
  assert.equal(replay.value.run_id,refresh.run_id);
  // Current human control uses the same supervisor, retaining source on Stop.
  const stopped=await submit(record,{action:"refresh"});
  const stop=await call({action:"execution",request:stopped.request,control:{action:"stop",run_id:stopped.run_id}});
  assert.equal(stop.value.accepted,true);
  await wait(stopped,"cancelled"); evidence.cancelled=true; save();
  record=await inspect(record.operation);
  record=(await perform(record,{action:"refresh"})).value;
  record=(await perform(record,{action:"preserve"})).value;
  const review=(await perform(record,{action:"review_removal"})).value;
  assert.deepEqual(review.issues,[]);
  record=await inspect(record.operation);
  record=(await perform(record,{action:"approve_removal",review:review.id})).value;
  record=(await perform(record,{action:"finish"})).value;
  assert.equal(record.stage,"closed"); assert.equal(fs.existsSync(fixture.checkout),false);
  const history=(await call({action:"history",location:fixture.location})).value;
  assert.equal(history.record.stage,"closed"); assert(fs.existsSync(history.report));
  const capture=fs.readdirSync(record.preservation_directory).map(name=>path.join(record.preservation_directory,name)).find(dir=>fs.existsSync(path.join(dir,"verified-restore","payload")));
  assert(capture); assert.equal(fs.statSync(path.join(capture,"verified-restore","payload")).size,fixture.payload_bytes);
  assert.equal(createHash("sha256").update(fs.readFileSync(path.join(capture,"verified-restore","payload"))).digest("hex"),fixture.payload_sha256);
  assert.equal(fs.readFileSync(path.join(capture,"verified-restore","unique"),"utf8"),"unique fixture information\n");
  const output=await call({action:"execution",request:actions.at(-1).request,control:{action:"read",run_id:actions.at(-1).run_id,content:"output",output_size:2000}});
  assert.equal(output.kind,"execution"); assert.equal(output.value.kind,"content");
  evidence.record=record; evidence.history=history; evidence.output_read=true; save();
} finally {
  if (!client) client=await connect();
  const cleanup=[];
  for(const intent of intents.filter(intent=>!actions.some(action=>action.request===intent.request))) {
    try {const reply=await call({action:"inspect_action",request:intent.request}); assert.equal(reply.kind,"action"); actions.push(reply.value); save();}
    catch(error) {cleanup.push({request:intent.request,error:String(error)});}
  }
  for(const action of actions) {
    try {
      let reply=await call({action:"execution",request:action.request,control:{action:"inspect",run_id:action.run_id}});
      if(!terminal.has(reply.value.run.state)) {
        await call({action:"execution",request:action.request,control:{action:"stop",run_id:action.run_id}});
        const deadline=Date.now()+30000;
        do {await new Promise(resolve=>setTimeout(resolve,100));reply=await call({action:"execution",request:action.request,control:{action:"inspect",run_id:action.run_id}});} while(!terminal.has(reply.value.run.state)&&Date.now()<deadline);
      }
      assert(terminal.has(reply.value.run.state)); cleanup.push({run:action.run_id,state:reply.value.run.state,complete:reply.value.run.complete});
    } catch(error) {cleanup.push({run:action.run_id,error:String(error)});}
  }
  fs.writeFileSync(path.join(root,"sdk-cleanup.json"),JSON.stringify(cleanup,null,2));
  client.close();
  assert(cleanup.every(item=>!item.error&&(item.complete||item.state==="failed")),JSON.stringify(cleanup));
}
