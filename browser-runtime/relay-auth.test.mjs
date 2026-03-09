import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import {
  deriveRelayToken,
  resolveRelayAcceptedTokens,
  RELAY_BROWSER_IDENTITY,
  RELAY_AUTH_HEADER,
  probeAuthenticatedIronClawRelay
} from "./relay-auth.mjs";
import { installExtensionRelay } from "./relay.mjs";

function listen(server) {
  return new Promise((resolve, reject) => {
    server.listen(0, "127.0.0.1", () => resolve(server.address().port));
    server.once("error", reject);
  });
}

function fetchStatus(url, token) {
  return new Promise((resolve, reject) => {
    const headers = token ? { [RELAY_AUTH_HEADER]: token } : {};
    http.get(url, { headers }, (res) => {
      const chunks = [];
      res.on("data", (chunk) => chunks.push(chunk));
      res.on("end", () => {
        resolve({
          status: res.statusCode,
          body: Buffer.concat(chunks).toString("utf8")
        });
      });
    }).on("error", reject);
  });
}

test("deriveRelayToken is deterministic per port", () => {
  const a1 = deriveRelayToken("test-gateway-token", 24242);
  const a2 = deriveRelayToken("test-gateway-token", 24242);
  const b = deriveRelayToken("test-gateway-token", 24243);
  assert.equal(a1, a2);
  assert.notEqual(a1, b);
  assert.match(a1, /^[0-9a-f]{64}$/);
});

test("resolveRelayAcceptedTokens includes derived and raw gateway tokens", () => {
  const accepted = resolveRelayAcceptedTokens({
    port: 24242,
    relayToken: "",
    gatewayToken: "test-gateway-token"
  });
  assert.equal(accepted.has("test-gateway-token"), true);
  assert.equal(accepted.has(deriveRelayToken("test-gateway-token", 24242)), true);
});

test("relay accepts raw gateway token and derived token for /json/version", async () => {
  let relay;
  const server = http.createServer((req, res) => {
    if (relay?.handleHttp(req, res)) {
      return;
    }
    res.writeHead(404);
    res.end("not found");
  });
  const port = await listen(server);
  const baseUrl = `http://127.0.0.1:${port}`;
  relay = installExtensionRelay(undefined, {
    baseUrl,
    host: "127.0.0.1",
    port,
    relayToken: "",
    gatewayToken: "test-gateway-token"
  });

  const derived = deriveRelayToken("test-gateway-token", port);
  const res1 = await fetchStatus(`${baseUrl}/json/version`, derived);
  const res2 = await fetchStatus(`${baseUrl}/json/version`, "test-gateway-token");
  assert.equal(res1.status, 200);
  assert.equal(res2.status, 200);
  assert.match(res1.body, new RegExp(RELAY_BROWSER_IDENTITY));
  server.close();
});

test("probeAuthenticatedIronClawRelay recognizes an existing authenticated IronClaw relay", async () => {
  const server = http.createServer((req, res) => {
    if (req.url?.startsWith("/json/version")) {
      const header = req.headers[RELAY_AUTH_HEADER];
      const token = Array.isArray(header) ? header[0] : header;
      if (!token) {
        res.writeHead(401);
        res.end("Unauthorized");
        return;
      }
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ Browser: RELAY_BROWSER_IDENTITY }));
      return;
    }
    res.writeHead(200, { "Content-Type": "text/plain; charset=utf-8" });
    res.end("OK");
  });
  const port = await listen(server);

  try {
    const ok = await probeAuthenticatedIronClawRelay({
      baseUrl: `http://127.0.0.1:${port}`,
      relayAuthToken: "test-relay-token"
    });
    assert.equal(ok, true);
  } finally {
    server.close();
  }
});
