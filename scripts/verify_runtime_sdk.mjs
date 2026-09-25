// Real bridge/Rust/TypeScript lifecycle journey. Only a marker-owned CLI fixture.
import {JcodeClient, HarnessError} from "../sdk/typescript/dist/index.js";
import {spawn, execFile} from "node:child_process";
import {promisify} from "node:util";
import {randomUUID, createHash} from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import assert from "node:assert/strict";
const exec = promisify(execFile);
const root = process.env.JCODE_WP08_NATIVE_ROOT;
assert(root && fs.existsSync(path.join(root,"owned-fixture.json")));
const binary = process.env.JCODE_WP08_BINARY, api = process.env.JCODE_WP08_API_SOCKET;
const native = process.env.JCODE_SOCKET, bridgeBinary = process.env.JCODE_WP08_BRIDGE, rustBinary = process.env.JCODE_WP08_RUST_PROBE;
assert(binary && api && native && bridgeBinary && rustBinary);
const evidence = {calls:[], cleanup_errors:[]};
const save = () => fs.writeFileSync(path.join(root,"sdk-runtime-result.json"),JSON.stringify(evidence,null,2));
const read = name => JSON.parse(fs.readFileSync(path.join(root,name),"utf8"));
const write = (name,value) => fs.writeFileSync(path.join(root,name),JSON.stringify(value,null,2));
async function cli(...args) {
  const result = await exec(binary,["--no-update","--socket",native,"runtime",...args,"--json"],{timeout:45000});
  const parsed=JSON.parse(result.stdout);evidence.calls.push({cli:args,result:parsed});save();return parsed;
}
async function rust(mode) {
  const result=await exec(rustBinary,["--ignored","--exact","runtime_native_sdk_fixture","--nocapture"],{env:{...process.env,JCODE_WP08_SDK_MODE:mode},timeout:45000});
  fs.writeFileSync(path.join(root,`rust-${mode}.log`),result.stdout+result.stderr);
  assert(result.stdout.includes("1 passed"));
}
let client;
const bridgeLog=fs.openSync(path.join(root,"runtime-bridge.log"),"w");
const bridge=spawn(bridgeBinary,[api,native],{stdio:["ignore",bridgeLog,bridgeLog]});
const bridgeExit=new Promise((resolve,reject)=>{bridge.once("exit",(code,signal)=>resolve({code,signal}));bridge.once("error",reject);});
const connect = () => JcodeClient.connect({socketPath:api,ensureRuntime:false,requestTimeoutMs:10000});
try {
  const deadline=Date.now()+15000;
  while (!fs.existsSync(api)) {assert.equal(bridge.exitCode,null);assert(Date.now()<deadline);await new Promise(resolve=>setTimeout(resolve,20));}
  await rust("review");
  const review=read("rust-runtime-review.json");assert.equal(review.kind,"review");
  client=await connect();
  const status=await client.runtimeControl({action:"status"});assert.equal(status.kind,"status");
  assert.deepEqual(status,read("rust-runtime-status.json"));
  const intent={request:randomUUID(),review:review.value.id};write("sdk-runtime-intent.json",intent);
  try {evidence.begin=await client.runtimeControl({action:"begin",...intent});assert.equal(evidence.begin.kind,"operation");}
  catch (error) {assert(error instanceof HarnessError && ["disconnected","timeout"].includes(error.code));evidence.uncertain={code:error.code,message:error.message};}
  client.close();client=null;
  const confirmed=await cli("confirm",intent.review,"--request",intent.request);
  intent.operation=confirmed.response.value.id;write("sdk-runtime-intent.json",intent);
  const terminal=await cli("wait",intent.operation,"--timeout-seconds","30");assert.equal(terminal.response.value.phase,"stopped");
  await rust("stopped");
  assert(!fs.existsSync(native),"SDK auto-started a stopped runtime");
  await cli("start");
  await rust("inspect");
  client=await connect();
  const historical=await client.runtimeControl({action:"inspect",operation:intent.operation});
  assert.deepEqual(historical,read("rust-runtime-inspect.json"));
  assert.equal(historical.value.phase,"stopped");evidence.rust_ts_history_equal=true;
  const second=await client.runtimeControl({action:"review",options:{strategy:"interrupt",independent:"stop",quiescence_timeout_seconds:10}});
  assert.equal(second.kind,"review");write("ts-runtime-review.json",second);
  client.close();client=null;
  await rust("begin");
  const rustIntent=read("rust-runtime-intent.json");
  const secondAck=await cli("confirm",rustIntent.review,"--request",rustIntent.request);
  await cli("wait",secondAck.response.value.id,"--timeout-seconds","30");
  await cli("start");
  evidence.passed=true;
} finally {
  client?.close();
  if (bridge.exitCode===null && bridge.signalCode===null) bridge.kill("SIGTERM");
  let timer;
  try {evidence.bridge_exit=await Promise.race([bridgeExit,new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error("Owned bridge did not exit")),10000);})]);}
  catch (error) {evidence.cleanup_errors.push(String(error));}
  finally {clearTimeout(timer);fs.closeSync(bridgeLog);}
  for (const [name,file] of [["bridge",bridgeBinary],["rust_probe",rustBinary],["typescript",new URL("../sdk/typescript/dist/runtime-control.js",import.meta.url)]]) {
    const hash=createHash("sha256");for await (const chunk of fs.createReadStream(file)) hash.update(chunk);evidence[name+"_sha256"]=hash.digest("hex");
  }
  save();
  assert.deepEqual(evidence.cleanup_errors,[]);
}
