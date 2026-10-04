import {test} from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import {JcodeClient, HarnessError, matchesWorkspaceResponse, requiredWorkspaceCapability, type WorkspaceRequest} from "../dist/index.js";
import {startMockHarness} from "./mock-harness.ts";

const cases = JSON.parse(fs.readFileSync(new URL("../../../crates/jcode-workspace-types/src/workspace_correlation.json", import.meta.url), "utf8"));
const allVersions = {catalog_version: 1, permissions_version: 1, checkout_version: 1, closeout_version: 2, management_version: 1, managed_rollout: false};

test("shared workspace correlation matrix matches Rust", () => {
  assert(cases.length >= 19);
  for (const item of cases) assert.equal(matchesWorkspaceResponse(item.request, item.response), item.accepted, item.name);
});

test("every workspace request names its negotiated contract", () => {
  assert.equal(requiredWorkspaceCapability({action: "status"}), "catalog");
  assert.equal(requiredWorkspaceCapability({action: "volumes"}), "checkout");
  assert.equal(requiredWorkspaceCapability({action: "operations", query: {}, after: null, limit: 5}), "management");
  assert.equal(requiredWorkspaceCapability({action: "permissions", request: {action: "grant", grant: "g"}}), "permissions");
});

async function withClient(capabilities: string[], onRequest: (request: any, send: (frame: any) => void) => void, body: (client: JcodeClient) => Promise<void>) {
  const server = await startMockHarness({capabilities, onRequest});
  const client = await JcodeClient.connect({socketPath: server.socketPath, ensureRuntime: false});
  try { await body(client); } finally { client.close(); await server.close(); }
}

test("workspace requests are sent only after exact native negotiation", async () => {
  const operations: WorkspaceRequest = {action: "operations", query: {kinds: ["closeout"]}, after: null, limit: 10};
  // No bridge capability: nothing is sent.
  let sent = 0;
  await withClient([], (request) => { sent++; }, async (client) => {
    await assert.rejects(() => client.workspace({action: "status"}), (error: unknown) => error instanceof HarnessError && error.code === "unsupported_capability");
  });
  assert.equal(sent, 0);
  // An older runtime without management support never receives the request.
  let workspace = 0;
  await withClient(["workspace_catalog_v1"], (request, send) => {
    if (request.req === "workspace_probe") send({v: 1, reply_to: request.id, ev: "workspace_capabilities", versions: {catalog_version: 1, permissions_version: 1, managed_rollout: false}});
    else workspace++;
  }, async (client) => {
    await assert.rejects(() => client.workspace(operations), (error: unknown) => error instanceof HarnessError && error.code === "unsupported_capability");
  });
  assert.equal(workspace, 0);
  // Closeout keeps its own route.
  await withClient(["workspace_catalog_v1"], () => { workspace++; }, async (client) => {
    await assert.rejects(() => client.workspace({action: "closeout", request: {action: "history", location: "l"}}), (error: unknown) => error instanceof HarnessError && error.code === "invalid_request");
  });
  assert.equal(workspace, 0);
});

test("workspace replies are correlated and domain rejections stay typed", async () => {
  for (const item of cases) {
    await withClient(["workspace_catalog_v1"], (request, send) => {
      if (request.req === "workspace_probe") { send({v: 1, reply_to: request.id, ev: "workspace_capabilities", versions: allVersions}); return; }
      assert.equal(request.req, "workspace");
      assert.deepEqual(request.request, item.request);
      send({v: 1, reply_to: request.id, ev: "workspace", response: item.response});
    }, async (client) => {
      if (item.accepted) assert.deepEqual(await client.workspace(item.request), item.response, item.name);
      else await assert.rejects(() => client.workspace(item.request), (error: unknown) => error instanceof HarnessError && error.code === "unexpected_reply", item.name);
    });
  }
});

test("session location inspection requires its capability and exact session", async () => {
  for (const version of [undefined, 1]) {
    let locations = 0;
    await withClient(["primary_control_v1"], (request, send) => {
      if (request.req === "primary_control_probe") { send({v: 1, reply_to: request.id, ev: "primary_control_capabilities", input_version: 1, location_version: 1, location_enabled: false, session_inspection_version: version}); return; }
      locations++;
      send({v: 1, reply_to: request.id, ev: "primary_location", response: {status: "session", view: {session: "other", location: null, legacy_working_dir: "/tmp", isolated_child: false, pending: [], catalog_revision: 1, catalog_issue: null}}});
    }, async (client) => {
      const call = client.primaryLocation({action: "inspect_session", session: "wanted"});
      await assert.rejects(() => call, (error: unknown) => error instanceof HarnessError && error.code === (version === 1 ? "unexpected_reply" : "unsupported_capability"));
    });
    assert.equal(locations, version === 1 ? 1 : 0);
  }
});

test("placement review requires its capability and correlates session and request", async () => {
  const proposal = {session: "wanted", working_dir: "/fixture/repo", catalog_revision: 3, candidates: [{placement: {kind: "standalone", root: "/fixture/repo"}, root: "/fixture/repo", name: "repo", project: null, broad: false}], default: 0};
  for (const version of [undefined, 1]) {
    let locations = 0;
    await withClient(["primary_control_v1"], (request, send) => {
      if (request.req === "primary_control_probe") { send({v: 1, reply_to: request.id, ev: "primary_control_capabilities", input_version: 1, location_version: 1, location_enabled: true, legacy_adoption_version: 1, session_placement_version: version}); return; }
      locations++;
      if (request.command.action === "propose_placement") { send({v: 1, reply_to: request.id, ev: "primary_location", response: {status: "proposal", proposal: {...proposal, session: request.command.session}}}); return; }
      send({v: 1, reply_to: request.id, ev: "primary_location", response: {status: "state", record: {operation: "other-request", input: {request: "other-request", session: "wanted", expected_session_revision: 0, expected_catalog_revision: 3, placement: {kind: "standalone", id: "l"}, cwd: "/fixture/repo"}, state: "complete"}}});
    }, async (client) => {
      const propose = client.primaryLocation({action: "propose_placement", session: "wanted"});
      const place = () => client.primaryLocation({action: "place", request: {request: "this-request", session: "wanted", working_dir: "/fixture/repo", expected_catalog_revision: 3, placement: {kind: "standalone", root: "/fixture/repo"}}});
      if (version === 1) {
        const response = await propose;
        assert.equal(response.status, "proposal");
        await assert.rejects(place, (error: unknown) => error instanceof HarnessError && error.code === "unexpected_reply");
      } else {
        await assert.rejects(() => propose, (error: unknown) => error instanceof HarnessError && error.code === "unsupported_capability");
        await assert.rejects(place, (error: unknown) => error instanceof HarnessError && error.code === "unsupported_capability");
      }
    });
    assert.equal(locations, version === 1 ? 2 : 0);
  }
});
