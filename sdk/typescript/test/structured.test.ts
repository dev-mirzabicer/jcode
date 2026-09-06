import { test } from "node:test";
import assert from "node:assert/strict";
import { JcodeClient, StructuredOutputError } from "../dist/index.js";
import { startMockHarness } from "./mock-harness.ts";

const schema = {
  type: "object",
  additionalProperties: false,
  required: ["summary", "count"],
  properties: {
    summary: { type: "string" },
    count: { type: "integer", minimum: 0 },
  },
} as const;

type Summary = { summary: string; count: number };

function sendTurn(send: (frame: any) => void, sessionId: string, text: string): void {
  send({ v: 1, ev: "message_accepted", session_id: sessionId });
  send({ v: 1, ev: "text_delta", session_id: sessionId, text });
  send({ v: 1, ev: "turn_done", session_id: sessionId });
}

async function startStructuredHarness(options: { onRequest: (request: any, send: (frame: any) => void) => void }, mode: "synthetic" | "reject" | "unsupported" = "synthetic") {
  return startMockHarness({
    capabilities: mode === "unsupported" ? ["sessions"] : ["sessions", "workflow_prompt_rendering"],
    onRequest(request, send) {
      if (request.req === "render_workflow_prompt") {
        if (mode === "reject") { send({ v: 1, reply_to: request.id, ev: "error", code: "internal", message: "SYNTHETIC_RENDER_FAILURE" }); return; }
        const w = request.workflow;
        const content = w.kind === "structured_initial" ? `SERVER_INITIAL\n${w.content}\n${w.schema}` : `SERVER_CORRECTION\n${w.schema}\n${w.error_lines}${w.previous_response}`;
        send({ v: 1, reply_to: request.id, ev: "workflow_prompt_rendered", session_id: request.session_id, content });
        return;
      }
      options.onRequest(request, send);
    },
  });
}

test("runStructured validates JSON Schema and returns parsed data", async () => {
  const prompts: string[] = [];
  const server = await startStructuredHarness({
    onRequest(request, send) {
      if (request.req !== "send_message") return;
      prompts.push(request.content);
      sendTurn(send, "s1", '```json\n{"summary":"done","count":2}\n```');
    },
  });
  const client = await JcodeClient.connect({ socketPath: server.socketPath });

  const result = await client.runStructured<Summary>("s1", "Summarize the work", { schema });

  assert.deepEqual(result.data, { summary: "done", count: 2 });
  assert.equal(result.text, '```json\n{"summary":"done","count":2}\n```');
  assert.equal(result.attempts.length, 1);
  assert.deepEqual(result.attempts[0].errors, []);
  assert.ok(prompts[0].startsWith("SERVER_INITIAL\nSummarize the work\n"));
  assert.match(prompts[0], /"additionalProperties": false/);

  await client.close();
  await server.close();
});

test("runStructured sends a corrective retry after schema validation fails", async () => {
  const prompts: string[] = [];
  const responses = ['{"summary":42,"count":2}', '{"summary":"fixed","count":2}'];
  const server = await startStructuredHarness({
    onRequest(request, send) {
      if (request.req !== "send_message") return;
      prompts.push(request.content);
      sendTurn(send, "s1", responses[prompts.length - 1]);
    },
  });
  const client = await JcodeClient.connect({ socketPath: server.socketPath });

  const result = await client.runStructured<Summary>("s1", "Return a summary", {
    schema,
    maxRetries: 1,
  });

  assert.deepEqual(result.data, { summary: "fixed", count: 2 });
  assert.equal(prompts.length, 2);
  assert.ok(prompts[1].startsWith("SERVER_CORRECTION\n"));
  assert.match(prompts[1], /summary/);
  assert.match(prompts[1], /must be string/);
  assert.match(prompts[1], /"summary":42/);
  assert.equal(result.attempts.length, 2);
  assert.equal(result.attempts[0].errors[0].keyword, "type");
  assert.deepEqual(result.attempts[1].errors, []);

  await client.close();
  await server.close();
});

test("runStructured rejects with validation details after bounded retries are exhausted", async () => {
  const prompts: string[] = [];
  const server = await startStructuredHarness({
    onRequest(request, send) {
      if (request.req !== "send_message") return;
      prompts.push(request.content);
      sendTurn(send, "s1", "not json");
    },
  });
  const client = await JcodeClient.connect({ socketPath: server.socketPath });

  await assert.rejects(
    () => client.runStructured<Summary>("s1", "Return a summary", { schema, maxRetries: 1 }),
    (error: unknown) => {
      assert.ok(error instanceof StructuredOutputError);
      assert.equal(error.code, "structured_output_invalid");
      assert.equal(error.attempts.length, 2);
      assert.equal(error.validationErrors[0].keyword, "parse");
      assert.match(error.message, /after 2 attempts/);
      return true;
    },
  );
  assert.equal(prompts.length, 2);

  await client.close();
  await server.close();
});

test("render failures and unsupported old servers do not send structured turns", async () => {
  for (const mode of ["reject", "unsupported"] as const) {
    let sent = 0;
    const server = await startStructuredHarness({ onRequest() { sent++; } }, mode);
    const client = await JcodeClient.connect({ socketPath: server.socketPath });
    try {
      await assert.rejects(() => client.runStructured("s1", "INPUT", { schema }), (error: any) => mode === "unsupported" ? error.code === "unsupported_capability" : error.message.includes("SYNTHETIC_RENDER_FAILURE"));
      assert.equal(sent, 0);
    } finally { await client.close(); await server.close(); }
  }
});

test("structured retry excerpts never split a Unicode surrogate pair", async () => {
  const { buildStructuredCorrectionPrompt } = await import("../dist/structured.js");
  const request = buildStructuredCorrectionPrompt(schema, { attempt: 1, text: "😀x".repeat(4001), errors: [] });
  assert.equal(request.kind, "structured_correction");
  if (request.kind !== "structured_correction") throw new Error("wrong request kind");
  assert.ok(!/[\uD800-\uDFFF]/u.test(request.previous_response));
  assert.ok(request.previous_response.endsWith("truncated 8004 chars"));
});

test("malformed workflow replies fail before sending a structured turn", async () => {
  let sent = 0;
  const server = await startMockHarness({
    capabilities: ["sessions", "workflow_prompt_rendering"],
    onRequest(request, send) {
      if (request.req === "render_workflow_prompt") {
        send({ v: 1, reply_to: request.id, ev: "workflow_prompt_rendered", session_id: "s1" });
      } else sent++;
    },
  });
  const client = await JcodeClient.connect({ socketPath: server.socketPath });
  try {
    await assert.rejects(() => client.runStructured("s1", "INPUT", { schema }), (error: any) => error.code === "unexpected_reply");
    assert.equal(sent, 0);
  } finally { await client.close(); await server.close(); }
});
