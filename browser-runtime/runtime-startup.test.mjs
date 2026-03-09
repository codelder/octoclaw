import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";

import { RELAY_AUTH_HEADER, RELAY_BROWSER_IDENTITY } from "./relay-auth.mjs";
import {
  listenWithCompatibleRuntimeProbe,
  resolveRelayProbeToken
} from "./runtime-startup.mjs";

function listen(server) {
  return new Promise((resolve, reject) => {
    server.listen(0, "127.0.0.1", () => resolve(server.address().port));
    server.once("error", reject);
  });
}

test("resolveRelayProbeToken prefers explicit relay token over derived gateway token", () => {
  const token = resolveRelayProbeToken({
    port: 24242,
    relayToken: "explicit-token",
    gatewayToken: "gateway-token"
  });
  assert.equal(token, "explicit-token");
});

test("listenWithCompatibleRuntimeProbe reuses an existing authenticated IronClaw relay", async () => {
  const occupied = http.createServer((req, res) => {
    if (req.url?.startsWith("/json/version")) {
      const header = req.headers[RELAY_AUTH_HEADER];
      const token = Array.isArray(header) ? header[0] : header;
      if (token !== "relay-token") {
        res.writeHead(401);
        res.end("Unauthorized");
        return;
      }
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ Browser: RELAY_BROWSER_IDENTITY }));
      return;
    }
    res.writeHead(200);
    res.end("OK");
  });
  const port = await listen(occupied);

  const candidate = http.createServer((_req, res) => {
    res.writeHead(500);
    res.end("should not bind");
  });

  try {
    let reuseProbe;
    const result = await listenWithCompatibleRuntimeProbe({
      server: candidate,
      host: "127.0.0.1",
      port,
      relayToken: "relay-token",
      gatewayToken: "",
      onReuse: async (probe) => {
        reuseProbe = probe;
      }
    });
    assert.equal(result.reused, true);
    assert.equal(result.compatible, true);
    assert.equal(result.baseUrl, `http://127.0.0.1:${port}`);
    assert.equal(result.relayAuthToken, "relay-token");
    assert.equal(reuseProbe?.baseUrl, `http://127.0.0.1:${port}`);
    assert.equal(candidate.listening, false);
  } finally {
    candidate.close();
    occupied.close();
  }
});

test("listenWithCompatibleRuntimeProbe still fails on an unrelated occupied port", async () => {
  const occupied = http.createServer((_req, res) => {
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ Browser: "SomethingElse/1.0" }));
  });
  const port = await listen(occupied);

  const candidate = http.createServer((_req, res) => {
    res.writeHead(500);
    res.end("should not bind");
  });

  try {
    await assert.rejects(
      () =>
        listenWithCompatibleRuntimeProbe({
          server: candidate,
          host: "127.0.0.1",
          port,
          relayToken: "",
          gatewayToken: ""
        }),
      /EADDRINUSE/
    );
    assert.equal(candidate.listening, false);
  } finally {
    candidate.close();
    occupied.close();
  }
});
