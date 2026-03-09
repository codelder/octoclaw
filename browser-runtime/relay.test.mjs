import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import { once } from "node:events";
import WebSocket from "ws";
import { installExtensionRelay } from "./relay.mjs";

function listen(server) {
  return new Promise((resolve, reject) => {
    server.listen(0, "127.0.0.1", () => {
      resolve(server.address().port);
    });
    server.once("error", reject);
  });
}

function fetchJson(url) {
  return new Promise((resolve, reject) => {
    http
      .get(url, (res) => {
        const chunks = [];
        res.on("data", (chunk) => chunks.push(chunk));
        res.on("end", () => {
          try {
            resolve(JSON.parse(Buffer.concat(chunks).toString("utf8")));
          } catch (error) {
            reject(error);
          }
        });
      })
      .on("error", reject);
  });
}

function requestText(method, url, headers = {}) {
  return new Promise((resolve, reject) => {
    const req = http.request(url, { method, headers }, (res) => {
      const chunks = [];
      res.on("data", (chunk) => chunks.push(chunk));
      res.on("end", () => {
        resolve({
          statusCode: res.statusCode,
          body: Buffer.concat(chunks).toString("utf8"),
          headers: res.headers
        });
      });
    });
    req.on("error", reject);
    req.end();
  });
}

function waitForMessage(ws, timeoutMs = 5000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("timed out waiting for websocket message")), timeoutMs);
    ws.once("message", (data) => {
      try {
        clearTimeout(timer);
        resolve(JSON.parse(String(data)));
      } catch (error) {
        clearTimeout(timer);
        reject(error);
      }
    });
    ws.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
  });
}

function waitForError(ws, timeoutMs = 5000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("timed out waiting for websocket error")), timeoutMs);
    ws.once("error", (error) => {
      clearTimeout(timer);
      resolve(error);
    });
    ws.once("open", () => {
      clearTimeout(timer);
      reject(new Error("expected websocket error"));
    });
  });
}

function closeServer(server) {
  return new Promise((resolve, reject) => {
    server.close((error) => {
      if (error) {
        reject(error);
        return;
      }
      resolve();
    });
  });
}

function closeWs(ws) {
  return new Promise((resolve) => {
    if (!ws || ws.readyState === WebSocket.CLOSED) {
      resolve();
      return;
    }
    ws.once("close", resolve);
    ws.terminate();
  });
}

test("extension relay exposes attached tab to CDP clients and forwards commands", async () => {
  const token = "test-relay-token";
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
  relay = installExtensionRelay(server, {
    baseUrl,
    host: "127.0.0.1",
    port,
    relayToken: token
  });

  server.on("upgrade", (req, socket, head) => {
    if (relay.handleUpgrade(req, socket, head)) {
      return;
    }
    socket.destroy();
  });

  const extensionWs = new WebSocket(`ws://127.0.0.1:${port}/extension?token=${encodeURIComponent(token)}`);
  await once(extensionWs, "open");

  extensionWs.send(
    JSON.stringify({
      method: "forwardCDPEvent",
      params: {
        method: "Target.attachedToTarget",
        sessionId: "session-1",
        params: {
          sessionId: "session-1",
          targetInfo: {
            targetId: "target-1",
            type: "page",
            title: "Attached Tab",
            url: "https://example.com"
          },
          waitingForDebugger: false
        }
      }
    })
  );

  const version = await fetchJson(`${baseUrl}/json/version?token=${encodeURIComponent(token)}`);
  assert.equal(version.Browser, "IronClaw/extension-relay");

  const list = await fetchJson(`${baseUrl}/json/list?token=${encodeURIComponent(token)}`);
  assert.equal(list.length, 1);
  assert.equal(list[0].id, "target-1");

  const cdpWs = new WebSocket(`ws://127.0.0.1:${port}/cdp?token=${encodeURIComponent(token)}`);
  await once(cdpWs, "open");

  cdpWs.send(
    JSON.stringify({
      id: 1,
      method: "Target.setAutoAttach",
      params: { autoAttach: true, flatten: true }
    })
  );

  const attachedEvent = await waitForMessage(cdpWs);
  assert.equal(attachedEvent.method, "Target.attachedToTarget");
  assert.equal(attachedEvent.params.targetInfo.targetId, "target-1");

  cdpWs.send(
    JSON.stringify({
      id: 2,
      method: "Runtime.evaluate",
      sessionId: "session-1",
      params: { expression: "2 + 2" }
    })
  );

  const forwarded = await waitForMessage(extensionWs);
  assert.equal(forwarded.method, "forwardCDPCommand");
  assert.equal(forwarded.params.method, "Runtime.evaluate");

  extensionWs.send(
    JSON.stringify({
      id: forwarded.id,
      result: { result: { type: "number", value: 4 } }
    })
  );

  const response = await waitForMessage(cdpWs);
  assert.equal(response.id, 2);
  assert.equal(response.result.result.value, 4);

  await closeWs(cdpWs);
  await closeWs(extensionWs);
  await closeServer(server);
});

test("extension relay keeps attached targets across brief extension reconnects", async () => {
  const token = "test-relay-token";
  process.env.IRONCLAW_EXTENSION_RELAY_RECONNECT_GRACE_MS = "500";
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
  relay = installExtensionRelay(server, {
    baseUrl,
    host: "127.0.0.1",
    port,
    relayToken: token
  });

  server.on("upgrade", (req, socket, head) => {
    if (relay.handleUpgrade(req, socket, head)) {
      return;
    }
    socket.destroy();
  });

  const extensionWs1 = new WebSocket(`ws://127.0.0.1:${port}/extension?token=${encodeURIComponent(token)}`);
  await once(extensionWs1, "open");
  extensionWs1.send(
    JSON.stringify({
      method: "forwardCDPEvent",
      params: {
        method: "Target.attachedToTarget",
        sessionId: "session-1",
        params: {
          sessionId: "session-1",
          targetInfo: {
            targetId: "target-1",
            type: "page",
            title: "Attached Tab",
            url: "https://example.com"
          },
          waitingForDebugger: false
        }
      }
    })
  );

  extensionWs1.close();
  await once(extensionWs1, "close");

  const listDuringGrace = await fetchJson(`${baseUrl}/json/list?token=${encodeURIComponent(token)}`);
  assert.equal(listDuringGrace.length, 1);
  assert.equal(listDuringGrace[0].id, "target-1");

  const extensionWs2 = new WebSocket(`ws://127.0.0.1:${port}/extension?token=${encodeURIComponent(token)}`);
  await once(extensionWs2, "open");

  const cdpWs = new WebSocket(`ws://127.0.0.1:${port}/cdp?token=${encodeURIComponent(token)}`);
  await once(cdpWs, "open");
  cdpWs.send(
    JSON.stringify({
      id: 1,
      method: "Target.setAutoAttach",
      params: { autoAttach: true, flatten: true }
    })
  );

  const attachedEvent = await waitForMessage(cdpWs);
  assert.equal(attachedEvent.method, "Target.attachedToTarget");
  assert.equal(attachedEvent.params.targetInfo.targetId, "target-1");

  await closeWs(cdpWs);
  await closeWs(extensionWs2);
  await closeServer(server);
  delete process.env.IRONCLAW_EXTENSION_RELAY_RECONNECT_GRACE_MS;
});

test("extension relay exposes OpenClaw-compatible HTTP routes for activate, close, root, and CORS preflight", async () => {
  const token = "test-relay-token";
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
  relay = installExtensionRelay(server, {
    baseUrl,
    host: "127.0.0.1",
    port,
    relayToken: token
  });

  server.on("upgrade", (req, socket, head) => {
    if (relay.handleUpgrade(req, socket, head)) {
      return;
    }
    socket.destroy();
  });

  const extensionWs = new WebSocket(`ws://127.0.0.1:${port}/extension?token=${encodeURIComponent(token)}`);
  await once(extensionWs, "open");

  const headRoot = await requestText("HEAD", `${baseUrl}/`);
  assert.equal(headRoot.statusCode, 200);

  const getRoot = await requestText("GET", `${baseUrl}/`);
  assert.equal(getRoot.statusCode, 200);
  assert.equal(getRoot.body, "OK");

  const corsPreflight = await requestText("OPTIONS", `${baseUrl}/json/list`, {
    Origin: "chrome-extension://test-extension-id",
    "Access-Control-Request-Headers": "content-type, x-ironclaw-relay-token"
  });
  assert.equal(corsPreflight.statusCode, 204);
  assert.equal(corsPreflight.headers["access-control-allow-origin"], "chrome-extension://test-extension-id");

  const activateForwardedPromise = waitForMessage(extensionWs);
  const activate = await requestText(
    "GET",
    `${baseUrl}/json/activate/${encodeURIComponent("target-1")}?token=${encodeURIComponent(token)}`
  );
  assert.equal(activate.statusCode, 200);
  assert.equal(activate.body, "OK");
  const activateForwarded = await activateForwardedPromise;
  assert.equal(activateForwarded.method, "forwardCDPCommand");
  assert.equal(activateForwarded.params.method, "Target.activateTarget");
  assert.equal(activateForwarded.params.params.targetId, "target-1");
  extensionWs.send(JSON.stringify({ id: activateForwarded.id, result: {} }));

  const closeForwardedPromise = waitForMessage(extensionWs);
  const close = await requestText(
    "PUT",
    `${baseUrl}/json/close/${encodeURIComponent("target-1")}`,
    { "x-ironclaw-relay-token": token }
  );
  assert.equal(close.statusCode, 200);
  assert.equal(close.body, "OK");
  const closeForwarded = await closeForwardedPromise;
  assert.equal(closeForwarded.method, "forwardCDPCommand");
  assert.equal(closeForwarded.params.method, "Target.closeTarget");
  assert.equal(closeForwarded.params.params.targetId, "target-1");
  extensionWs.send(JSON.stringify({ id: closeForwarded.id, result: {} }));

  const badEncoding = await requestText(
    "GET",
    `${baseUrl}/json/activate/%E0%A4%A?token=${encodeURIComponent(token)}`
  );
  assert.equal(badEncoding.statusCode, 400);
  assert.match(badEncoding.body, /invalid targetId encoding/i);

  await closeWs(extensionWs);
  await closeServer(server);
});

test("extension relay rejects CDP access without relay auth token", async () => {
  const token = "test-relay-token";
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
  relay = installExtensionRelay(server, {
    baseUrl,
    host: "127.0.0.1",
    port,
    relayToken: token
  });

  server.on("upgrade", (req, socket, head) => {
    if (relay.handleUpgrade(req, socket, head)) {
      return;
    }
    socket.destroy();
  });

  const version = await requestText("GET", `${baseUrl}/json/version`);
  assert.equal(version.statusCode, 401);
  assert.match(version.body, /unauthorized/i);

  const cdpWs = new WebSocket(`ws://127.0.0.1:${port}/cdp`);
  const err = await waitForError(cdpWs);
  assert.match(String(err?.message ?? err), /401/);

  await closeServer(server);
});

test("extension relay rejects non-extension CORS preflight and returns CORS headers for extension origins", async () => {
  const token = "test-relay-token";
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
  relay = installExtensionRelay(server, {
    baseUrl,
    host: "127.0.0.1",
    port,
    relayToken: token
  });

  const deniedPreflight = await requestText("OPTIONS", `${baseUrl}/json/version`, {
    Origin: "http://example.com",
    "Access-Control-Request-Headers": "x-ironclaw-relay-token"
  });
  assert.equal(deniedPreflight.statusCode, 403);

  const allowedGet = await requestText("GET", `${baseUrl}/json/version?token=${encodeURIComponent(token)}`, {
    Origin: "chrome-extension://abcdefghijklmnop"
  });
  assert.equal(allowedGet.statusCode, 200);
  assert.equal(allowedGet.headers["access-control-allow-origin"], "chrome-extension://abcdefghijklmnop");

  await closeServer(server);
});

test("extension relay rejects a second live extension websocket with 409", async () => {
  const token = "test-relay-token";
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
  relay = installExtensionRelay(server, {
    baseUrl,
    host: "127.0.0.1",
    port,
    relayToken: token
  });

  server.on("upgrade", (req, socket, head) => {
    if (relay.handleUpgrade(req, socket, head)) {
      return;
    }
    socket.destroy();
  });

  const extensionWs1 = new WebSocket(`ws://127.0.0.1:${port}/extension?token=${encodeURIComponent(token)}`);
  await once(extensionWs1, "open");

  const extensionWs2 = new WebSocket(`ws://127.0.0.1:${port}/extension?token=${encodeURIComponent(token)}`);
  const err = await waitForError(extensionWs2);
  assert.match(String(err?.message ?? err), /409/);

  await closeWs(extensionWs1);
  await closeServer(server);
});

test("extension relay allows immediate reconnect when prior extension socket is closing", async () => {
  const token = "test-relay-token";
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
  relay = installExtensionRelay(server, {
    baseUrl,
    host: "127.0.0.1",
    port,
    relayToken: token
  });

  server.on("upgrade", (req, socket, head) => {
    if (relay.handleUpgrade(req, socket, head)) {
      return;
    }
    socket.destroy();
  });

  const extensionWs1 = new WebSocket(`ws://127.0.0.1:${port}/extension?token=${encodeURIComponent(token)}`);
  await once(extensionWs1, "open");
  extensionWs1.close();

  const extensionWs2 = new WebSocket(`ws://127.0.0.1:${port}/extension?token=${encodeURIComponent(token)}`);
  await once(extensionWs2, "open");

  await closeWs(extensionWs2);
  await closeServer(server);
});
