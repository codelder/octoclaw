import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import path from "node:path";
import os from "node:os";
import fs from "node:fs/promises";
import { once } from "node:events";
import WebSocket from "ws";
import { chromium } from "playwright";
import { installExtensionRelay } from "./relay.mjs";

function listen(server) {
  return new Promise((resolve, reject) => {
    server.listen(0, "127.0.0.1", () => {
      resolve(server.address().port);
    });
    server.once("error", reject);
  });
}

function fetchJson(url, token = "") {
  return new Promise((resolve, reject) => {
    const headers = token ? { "x-ironclaw-relay-token": token } : {};
    http
      .get(url, { headers }, (res) => {
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

async function waitFor(fn, timeoutMs = 15000, intervalMs = 150) {
  const started = Date.now();
  let lastError = null;
  while (Date.now() - started < timeoutMs) {
    try {
      return await fn();
    } catch (error) {
      lastError = error;
      await new Promise((resolve) => setTimeout(resolve, intervalMs));
    }
  }
  throw lastError || new Error("timed out waiting for condition");
}

async function waitForServiceWorker(context) {
  const existing = context.serviceWorkers();
  if (existing.length > 0) {
    return existing[0];
  }
  return await context.waitForEvent("serviceworker");
}

async function readUntil(ws, predicate, timeoutMs = 15000) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    const message = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("timed out waiting for websocket message")), timeoutMs);
      ws.once("message", (data) => {
        clearTimeout(timer);
        try {
          resolve(JSON.parse(String(data)));
        } catch (error) {
          reject(error);
        }
      });
      ws.once("error", (error) => {
        clearTimeout(timer);
        reject(error);
      });
    });
    if (predicate(message)) {
      return message;
    }
  }
  throw new Error("predicate did not match before timeout");
}

async function maybeReadUntil(ws, predicate, timeoutMs = 3000) {
  try {
    return await readUntil(ws, predicate, timeoutMs);
  } catch (error) {
    if (String(error).includes("timed out")) {
      return null;
    }
    throw error;
  }
}

test(
  "browser relay attaches a real tab through the extension and evaluates via CDP",
  { timeout: 120000 },
  async () => {
    const token = "test-relay-browser-token";
    let relay;

    const relayServer = http.createServer((req, res) => {
      if (relay?.handleHttp(req, res)) {
        return;
      }
      res.writeHead(404);
      res.end("not found");
    });

    const relayPort = await listen(relayServer);
    const relayBaseUrl = `http://127.0.0.1:${relayPort}`;
    relay = installExtensionRelay(relayServer, {
      baseUrl: relayBaseUrl,
      host: "127.0.0.1",
      port: relayPort,
      relayToken: token
    });

    relayServer.on("upgrade", (req, socket, head) => {
      if (relay.handleUpgrade(req, socket, head)) {
        return;
      }
      socket.destroy();
    });

    const pageServer = http.createServer((req, res) => {
      res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
      res.end(`<!doctype html><html><head><title>Relay Browser Test</title></head><body><h1>relay ok</h1><p>${req.url}</p></body></html>`);
    });
    const pagePort = await listen(pageServer);
    const pageUrl = `http://127.0.0.1:${pagePort}/target`;

    const extensionPath = path.resolve("browser-runtime/chrome-extension");
    const userDataDir = await fs.mkdtemp(path.join(os.tmpdir(), "ironclaw-relay-browser-"));

    let context;
    try {
      context = await chromium.launchPersistentContext(userDataDir, {
        headless: false,
        args: [
          `--disable-extensions-except=${extensionPath}`,
          `--load-extension=${extensionPath}`
        ]
      });

      const serviceWorker = await waitForServiceWorker(context);
      const extensionId = new URL(serviceWorker.url()).host;

      await serviceWorker.evaluate(
        async ({ relayBaseUrl, token }) => {
          await chrome.storage.local.set({
            relayUrl: relayBaseUrl,
            relayToken: token
          });
        },
        { relayBaseUrl, token }
      );

      const targetPage = await context.newPage();
      await targetPage.goto(pageUrl);
      await targetPage.bringToFront();

      const optionsPage = await context.newPage();
      await optionsPage.goto(`chrome-extension://${extensionId}/options.html`);
      await targetPage.bringToFront();

      const attachResult = await optionsPage.evaluate(async (url) => {
        return await chrome.runtime.sendMessage({ type: "attachActiveTab", url });
      }, pageUrl);
      assert.equal(attachResult.ok, true, JSON.stringify(attachResult));
      assert.equal(attachResult.attached, true);
      assert.equal(typeof attachResult.sessionId, "string");

      const list = await waitFor(async () => {
        const payload = await fetchJson(`${relayBaseUrl}/json/list`, token);
        assert.equal(payload.length, 1);
        assert.equal(payload[0].title, "Relay Browser Test");
        assert.equal(payload[0].url, pageUrl);
        return payload;
      });
      assert.equal(list[0].id.startsWith("target-"), true);

      const cdpWs = new WebSocket(`ws://127.0.0.1:${relayPort}/cdp?token=${encodeURIComponent(token)}`);
      await once(cdpWs, "open");

      cdpWs.send(
        JSON.stringify({
          id: 1,
          method: "Target.setAutoAttach",
          params: { autoAttach: true, flatten: true }
        })
      );

      const attachedEvent = await readUntil(cdpWs, (message) => message.method === "Target.attachedToTarget");
      assert.equal(attachedEvent.params.targetInfo.title, "Relay Browser Test");

      cdpWs.send(
        JSON.stringify({
          id: 2,
          method: "Runtime.evaluate",
          sessionId: attachResult.sessionId,
          params: { expression: "document.title" }
        })
      );

      const evalResponse = await readUntil(cdpWs, (message) => message.id === 2);
      assert.equal(evalResponse.result.result.value, "Relay Browser Test");

      cdpWs.close();
      await optionsPage.close();
      await targetPage.close();
    } finally {
      await context?.close().catch(() => {});
      pageServer.close();
      relayServer.close();
      await fs.rm(userDataDir, { recursive: true, force: true });
    }
  }
);

test(
  "browser relay remains usable after page navigation and possible debugger reattach",
  { timeout: 120000 },
  async () => {
    const token = "test-relay-browser-nav-token";
    let relay;

    const relayServer = http.createServer((req, res) => {
      if (relay?.handleHttp(req, res)) {
        return;
      }
      res.writeHead(404);
      res.end("not found");
    });

    const relayPort = await listen(relayServer);
    const relayBaseUrl = `http://127.0.0.1:${relayPort}`;
    relay = installExtensionRelay(relayServer, {
      baseUrl: relayBaseUrl,
      host: "127.0.0.1",
      port: relayPort,
      relayToken: token
    });

    relayServer.on("upgrade", (req, socket, head) => {
      if (relay.handleUpgrade(req, socket, head)) {
        return;
      }
      socket.destroy();
    });

    const pageServer = http.createServer((req, res) => {
      const isAfter = req.url?.includes("after-nav");
      res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
      res.end(
        `<!doctype html><html><head><title>${
          isAfter ? "Relay After Nav" : "Relay Before Nav"
        }</title></head><body><h1>${req.url}</h1></body></html>`
      );
    });
    const pagePort = await listen(pageServer);
    const beforeUrl = `http://127.0.0.1:${pagePort}/before-nav`;
    const afterUrl = `http://127.0.0.1:${pagePort}/after-nav`;

    const extensionPath = path.resolve("browser-runtime/chrome-extension");
    const userDataDir = await fs.mkdtemp(path.join(os.tmpdir(), "ironclaw-relay-nav-"));

    let context;
    try {
      context = await chromium.launchPersistentContext(userDataDir, {
        headless: false,
        args: [
          `--disable-extensions-except=${extensionPath}`,
          `--load-extension=${extensionPath}`
        ]
      });

      const serviceWorker = await waitForServiceWorker(context);
      const extensionId = new URL(serviceWorker.url()).host;

      await serviceWorker.evaluate(
        async ({ relayBaseUrl, token }) => {
          await chrome.storage.local.set({
            relayUrl: relayBaseUrl,
            relayToken: token
          });
        },
        { relayBaseUrl, token }
      );

      const targetPage = await context.newPage();
      await targetPage.goto(beforeUrl);
      await targetPage.bringToFront();

      const optionsPage = await context.newPage();
      await optionsPage.goto(`chrome-extension://${extensionId}/options.html`);
      await targetPage.bringToFront();

      const attachResult = await optionsPage.evaluate(async (url) => {
        return await chrome.runtime.sendMessage({ type: "attachActiveTab", url });
      }, beforeUrl);
      assert.equal(attachResult.ok, true, JSON.stringify(attachResult));
      assert.equal(typeof attachResult.sessionId, "string");

      const cdpWs = new WebSocket(`ws://127.0.0.1:${relayPort}/cdp?token=${encodeURIComponent(token)}`);
      await once(cdpWs, "open");

      cdpWs.send(
        JSON.stringify({
          id: 1,
          method: "Target.setAutoAttach",
          params: { autoAttach: true, flatten: true }
        })
      );
      await readUntil(cdpWs, (message) => message.method === "Target.attachedToTarget");

      await targetPage.goto(afterUrl);

      await waitFor(async () => {
        const payload = await fetchJson(`${relayBaseUrl}/json/list`, token);
        assert.equal(payload.length, 1);
        assert.equal(payload[0].url, afterUrl);
        assert.equal(payload[0].title, "Relay After Nav");
        return payload;
      });

      const maybeReattached = await maybeReadUntil(
        cdpWs,
        (message) =>
          message.method === "Target.attachedToTarget" &&
          message.params?.targetInfo?.url === afterUrl,
        5000
      );
      const sessionId = maybeReattached?.params?.sessionId || attachResult.sessionId;

      cdpWs.send(
        JSON.stringify({
          id: 2,
          method: "Runtime.evaluate",
          sessionId,
          params: { expression: "document.title" }
        })
      );

      const evalResponse = await readUntil(cdpWs, (message) => message.id === 2);
      assert.equal(evalResponse.result.result.value, "Relay After Nav");

      const extensionState = await optionsPage.evaluate(async (url) => {
        return await chrome.runtime.sendMessage({ type: "extensionState", url });
      }, afterUrl);
      assert.equal(extensionState.ok, true);
      assert.equal(extensionState.attached, true);

      cdpWs.close();
      await optionsPage.close();
      await targetPage.close();
    } finally {
      await context?.close().catch(() => {});
      pageServer.close();
      relayServer.close();
      await fs.rm(userDataDir, { recursive: true, force: true });
    }
  }
);
