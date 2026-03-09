import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import fs from "node:fs/promises";
import { once } from "node:events";
import { spawn } from "node:child_process";

function listen(server) {
  return new Promise((resolve, reject) => {
    server.listen(0, "127.0.0.1", () => {
      resolve(server.address().port);
    });
    server.once("error", reject);
  });
}

async function reserveFreePort() {
  const server = http.createServer((_req, res) => {
    res.writeHead(200);
    res.end("reserved");
  });
  const port = await listen(server);
  await new Promise((resolve, reject) => server.close((error) => (error ? reject(error) : resolve())));
  return port;
}

async function waitFor(fn, timeoutMs = 20000, intervalMs = 150) {
  const start = Date.now();
  let lastError = null;
  while (Date.now() - start < timeoutMs) {
    try {
      return await fn();
    } catch (error) {
      lastError = error;
      await new Promise((resolve) => setTimeout(resolve, intervalMs));
    }
  }
  throw lastError || new Error("timed out waiting for condition");
}

async function fetchJson(url, options = {}) {
  const res = await fetch(url, options);
  const text = await res.text();
  if (!res.ok) {
    throw new Error(`${res.status} ${text}`);
  }
  return JSON.parse(text);
}

async function postJson(url, body) {
  return await fetchJson(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body)
  });
}

async function startRuntime(envOverrides = {}) {
  const port = await reserveFreePort();
  const child = spawn("node", ["server.mjs"], {
    cwd: path.resolve("browser-runtime"),
    env: {
      ...process.env,
      IRONCLAW_BROWSER_RUNTIME_HOST: "127.0.0.1",
      IRONCLAW_BROWSER_RUNTIME_PORT: String(port),
      IRONCLAW_BROWSER_RUNTIME_HEADLESS: "1",
      ...envOverrides
    },
    stdio: ["ignore", "pipe", "pipe"]
  });

  let stderr = "";
  child.stderr.on("data", (chunk) => {
    stderr += String(chunk);
  });

  const baseUrl = `http://127.0.0.1:${port}`;
  try {
    await waitFor(async () => {
      const status = await fetchJson(`${baseUrl}/_ironclaw/runtime`);
      assert.equal(status.identity, "IronClaw/browser-runtime");
      return status;
    });
  } catch (error) {
    child.kill("SIGTERM");
    await once(child, "exit").catch(() => {});
    throw new Error(`runtime failed to start: ${error}\n${stderr}`);
  }

  return {
    baseUrl,
    child,
    get stderr() {
      return stderr;
    }
  };
}

async function stopRuntime(runtime) {
  if (!runtime?.child || runtime.child.exitCode !== null) {
    return;
  }
  runtime.child.kill("SIGTERM");
  await once(runtime.child, "exit").catch(() => {});
}

async function createContractHtml() {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), "ironclaw-browser-contract-"));
  const filePath = path.join(dir, "contract.html");
  await fs.writeFile(
    filePath,
    `<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <title>Browser Contract Test</title>
    <style>
      body { font-family: sans-serif; }
      .spacer { height: 1200px; }
    </style>
  </head>
  <body data-clicked="no" data-hovered="no" data-last-key="">
    <h1>Browser Contract Test</h1>
    <button id="save-btn" aria-label="Save" onclick="document.body.dataset.clicked='yes'">Save</button>
    <input id="name-input" name="name" aria-label="Name" value="" />
    <input id="message-input" name="message" aria-label="Message" value="" />
    <input id="upload-input" type="file" aria-label="Upload file" />
    <select id="choice-select" name="choice" aria-label="Choice">
      <option value="a">Alpha</option>
      <option value="b">Beta</option>
    </select>
    <div id="hover-target" role="button" tabindex="0" aria-label="Hover target"
      onmouseover="document.body.dataset.hovered='yes'">Hover target</div>
    <button id="dialog-btn" aria-label="Open dialog" onclick="document.body.dataset.dialogResult = String(confirm('Proceed?'))">
      Open dialog
    </button>
    <div class="spacer"></div>
    <script>
      document.addEventListener('keydown', (event) => {
        document.body.dataset.lastKey = event.key;
      });
    </script>
  </body>
</html>`,
    "utf8"
  );
  return { dir, filePath };
}

function findAriaNode(snapshot, matcher, label) {
  const node = snapshot.nodes.find((entry) => matcher(entry));
  assert.ok(node, `expected aria snapshot to include node ${label}`);
  return node;
}

test(
  "browser server contract: snapshot endpoints and common act commands",
  { timeout: 120000 },
  async () => {
    const runtime = await startRuntime();
    const fixture = await createContractHtml();
    try {
      const opened = await postJson(`${runtime.baseUrl}/tabs/open`, {
        url: `file://${fixture.filePath}`
      });
      assert.equal(typeof opened.targetId, "string");

      const snapAria = await fetchJson(
        `${runtime.baseUrl}/snapshot?format=aria&refs=aria&targetId=${encodeURIComponent(opened.targetId)}`
      );
      assert.equal(snapAria.ok, true);
      assert.equal(snapAria.format, "aria");
      const saveButton = findAriaNode(snapAria, (entry) => entry.role === "button" && entry.name === "Save", 'button "Save"');
      const nameInput = findAriaNode(snapAria, (entry) => entry.role === "input" && entry.name === "Name", 'input "Name"');
      const choiceSelect = findAriaNode(
        snapAria,
        (entry) => entry.role === "select" && String(entry.name || "").includes("Alpha"),
        'select containing "Alpha"'
      );
      const hoverTarget = findAriaNode(
        snapAria,
        (entry) => entry.role === "element" && entry.name === "Hover target",
        '"Hover target" element'
      );

      const snapAi = await fetchJson(
        `${runtime.baseUrl}/snapshot?format=ai&refs=aria&targetId=${encodeURIComponent(opened.targetId)}`
      );
      assert.equal(snapAi.ok, true);
      assert.equal(snapAi.format, "ai");
      assert.match(snapAi.snapshot, /Interactive elements:/);
      assert.ok(snapAi.refs[saveButton.ref]);

      const click = await postJson(`${runtime.baseUrl}/act`, {
        targetId: opened.targetId,
        kind: "click",
        ref: saveButton.ref
      });
      assert.equal(click.ok, true);

      const type = await postJson(`${runtime.baseUrl}/act`, {
        targetId: opened.targetId,
        kind: "type",
        ref: nameInput.ref,
        text: "Alice"
      });
      assert.equal(type.ok, true);

      const press = await postJson(`${runtime.baseUrl}/act`, {
        targetId: opened.targetId,
        kind: "press",
        key: "Enter"
      });
      assert.equal(press.ok, true);

      const hover = await postJson(`${runtime.baseUrl}/act`, {
        targetId: opened.targetId,
        kind: "hover",
        ref: hoverTarget.ref
      });
      assert.equal(hover.ok, true);

      const select = await postJson(`${runtime.baseUrl}/act`, {
        targetId: opened.targetId,
        kind: "select",
        ref: choiceSelect.ref,
        values: ["b"]
      });
      assert.equal(select.ok, true);

      const fill = await postJson(`${runtime.baseUrl}/act`, {
        targetId: opened.targetId,
        kind: "fill",
        fields: [{ name: "message", value: "hello from fill" }]
      });
      assert.equal(fill.ok, true);

      const resize = await postJson(`${runtime.baseUrl}/act`, {
        targetId: opened.targetId,
        kind: "resize",
        width: 800,
        height: 600
      });
      assert.equal(resize.ok, true);
      assert.equal(resize.viewport.width, 800);
      assert.equal(resize.viewport.height, 600);

      const wait = await postJson(`${runtime.baseUrl}/act`, {
        targetId: opened.targetId,
        kind: "wait",
        timeMs: 5
      });
      assert.equal(wait.ok, true);

      const evaluated = await postJson(`${runtime.baseUrl}/act`, {
        targetId: opened.targetId,
        kind: "evaluate",
        fn: `() => ({
          clicked: document.body.dataset.clicked,
          hovered: document.body.dataset.hovered,
          lastKey: document.body.dataset.lastKey,
          name: document.querySelector('#name-input').value,
          message: document.querySelector('#message-input').value,
          choice: document.querySelector('#choice-select').value,
          viewportWidth: window.innerWidth
        })`
      });
      assert.equal(evaluated.ok, true);
      assert.equal(evaluated.result.clicked, "yes");
      assert.equal(evaluated.result.hovered, "yes");
      assert.equal(evaluated.result.lastKey, "Enter");
      assert.equal(evaluated.result.name, "Alice");
      assert.equal(evaluated.result.message, "hello from fill");
      assert.equal(evaluated.result.choice, "b");
      assert.equal(evaluated.result.viewportWidth, 800);
    } finally {
      await stopRuntime(runtime);
      await fs.rm(fixture.dir, { recursive: true, force: true });
    }
  }
);

test(
  "browser server contract: blocks act:evaluate and wait:fn when evaluate is disabled",
  { timeout: 120000 },
  async () => {
    const runtime = await startRuntime({
      IRONCLAW_BROWSER_RUNTIME_EVALUATE: "0"
    });
    const fixture = await createContractHtml();
    try {
      const opened = await postJson(`${runtime.baseUrl}/tabs/open`, {
        url: `file://${fixture.filePath}`
      });

      const waitResponse = await fetch(`${runtime.baseUrl}/act`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          targetId: opened.targetId,
          kind: "wait",
          fn: "() => true"
        })
      });
      assert.equal(waitResponse.status, 500);
      assert.match(await waitResponse.text(), /IRONCLAW_BROWSER_RUNTIME_EVALUATE/i);

      const evalResponse = await fetch(`${runtime.baseUrl}/act`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          targetId: opened.targetId,
          kind: "evaluate",
          fn: "() => 1"
        })
      });
      assert.equal(evalResponse.status, 400);
      assert.match(await evalResponse.text(), /IRONCLAW_BROWSER_RUNTIME_EVALUATE/i);
    } finally {
      await stopRuntime(runtime);
      await fs.rm(fixture.dir, { recursive: true, force: true });
    }
  }
);

test(
  "browser server contract: hooks, screenshot, pdf, and stop",
  { timeout: 120000 },
  async () => {
    const runtime = await startRuntime();
    const fixture = await createContractHtml();
    const uploadFile = path.join(fixture.dir, "upload.txt");
    await fs.writeFile(uploadFile, "upload payload", "utf8");
    try {
      const opened = await postJson(`${runtime.baseUrl}/tabs/open`, {
        url: `file://${fixture.filePath}`
      });
      assert.equal(typeof opened.targetId, "string");

      const snapAria = await fetchJson(
        `${runtime.baseUrl}/snapshot?format=aria&refs=aria&targetId=${encodeURIComponent(opened.targetId)}`
      );
      const uploadInput = findAriaNode(
        snapAria,
        (entry) => entry.role === "input" && entry.name === "Upload file",
        'input "Upload file"'
      );
      const dialogButton = findAriaNode(
        snapAria,
        (entry) => entry.role === "button" && entry.name === "Open dialog",
        'button "Open dialog"'
      );

      const uploadHook = await postJson(`${runtime.baseUrl}/hooks/file-chooser`, {
        targetId: opened.targetId,
        inputRef: uploadInput.ref,
        paths: [uploadFile]
      });
      assert.equal(uploadHook.ok, true);
      assert.equal(uploadHook.armed, false);

      const uploadEval = await postJson(`${runtime.baseUrl}/act`, {
        targetId: opened.targetId,
        kind: "evaluate",
        fn: `() => {
          const input = document.querySelector('#upload-input');
          return input.files.length > 0 ? input.files[0].name : '';
        }`
      });
      assert.equal(uploadEval.result, "upload.txt");

      const dialogHook = await postJson(`${runtime.baseUrl}/hooks/dialog`, {
        targetId: opened.targetId,
        accept: true
      });
      assert.equal(dialogHook.ok, true);
      assert.equal(dialogHook.armed, true);

      const dialogTrigger = await postJson(`${runtime.baseUrl}/act`, {
        targetId: opened.targetId,
        kind: "click",
        ref: dialogButton.ref
      });
      assert.equal(dialogTrigger.ok, true);

      const dialogEval = await waitFor(async () => {
        const value = await postJson(`${runtime.baseUrl}/act`, {
          targetId: opened.targetId,
          kind: "evaluate",
          fn: "() => document.body.dataset.dialogResult || ''"
        });
        assert.equal(value.ok, true);
        assert.notEqual(value.result, "");
        return value;
      });
      assert.equal(dialogEval.result, "true");

      const screenshot = await fetch(`${runtime.baseUrl}/screenshot`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          targetId: opened.targetId,
          element: uploadInput.ref,
          type: "jpeg"
        })
      });
      assert.equal(screenshot.status, 200);
      assert.equal(screenshot.headers.get("content-type"), "image/jpeg");
      const screenshotBytes = new Uint8Array(await screenshot.arrayBuffer());
      assert.ok(screenshotBytes.length > 100);

      const pdf = await fetch(`${runtime.baseUrl}/pdf`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ targetId: opened.targetId })
      });
      assert.equal(pdf.status, 200);
      assert.equal(pdf.headers.get("content-type"), "application/pdf");
      const pdfBytes = new Uint8Array(await pdf.arrayBuffer());
      assert.ok(pdfBytes.length > 1000);
      assert.equal(Buffer.from(pdfBytes.slice(0, 5)).toString("utf8"), "%PDF-");

      const stopped = await postJson(`${runtime.baseUrl}/stop`, {
        profile: "default"
      });
      assert.equal(stopped.ok, true);
      assert.equal(stopped.closed, true);

      const status = await fetchJson(`${runtime.baseUrl}/_ironclaw/runtime`);
      assert.equal(status.running, false);
    } finally {
      await stopRuntime(runtime);
      await fs.rm(fixture.dir, { recursive: true, force: true });
    }
  }
);

test(
  "browser server contract: act validation errors are explicit",
  { timeout: 120000 },
  async () => {
    const runtime = await startRuntime();
    const fixture = await createContractHtml();
    try {
      const opened = await postJson(`${runtime.baseUrl}/tabs/open`, {
        url: `file://${fixture.filePath}`
      });

      const missingSelectValues = await fetch(`${runtime.baseUrl}/act`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          targetId: opened.targetId,
          kind: "select",
          selector: "#choice-select"
        })
      });
      assert.equal(missingSelectValues.status, 400);
      assert.match(await missingSelectValues.text(), /values are required/i);

      const missingFillFields = await fetch(`${runtime.baseUrl}/act`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          targetId: opened.targetId,
          kind: "fill"
        })
      });
      assert.equal(missingFillFields.status, 400);
      assert.match(await missingFillFields.text(), /fields are required/i);

      const missingResizeArgs = await fetch(`${runtime.baseUrl}/act`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          targetId: opened.targetId,
          kind: "resize",
          width: 800
        })
      });
      assert.equal(missingResizeArgs.status, 400);
      assert.match(await missingResizeArgs.text(), /width and height are required/i);

      const missingWaitArgs = await fetch(`${runtime.baseUrl}/act`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          targetId: opened.targetId,
          kind: "wait"
        })
      });
      assert.equal(missingWaitArgs.status, 400);
      assert.match(await missingWaitArgs.text(), /wait requires at least one/i);

      const unsupportedKind = await fetch(`${runtime.baseUrl}/act`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          targetId: opened.targetId,
          kind: "scrollIntoView",
          selector: "body"
        })
      });
      assert.equal(unsupportedKind.status, 400);
      assert.match(await unsupportedKind.text(), /unsupported act kind/i);
    } finally {
      await stopRuntime(runtime);
      await fs.rm(fixture.dir, { recursive: true, force: true });
    }
  }
);

test(
  "browser server contract: targetId prefixes resolve like OpenClaw and ambiguous prefixes are rejected",
  { timeout: 120000 },
  async () => {
    const runtime = await startRuntime();
    const fixture = await createContractHtml();
    try {
      const first = await postJson(`${runtime.baseUrl}/tabs/open`, {
        url: `file://${fixture.filePath}`
      });
      const uniquePrefix = first.targetId.slice(0, 8);
      assert.ok(uniquePrefix.length > 0, "expected a non-empty unique prefix");

      const focused = await postJson(`${runtime.baseUrl}/tabs/focus`, {
        targetId: uniquePrefix
      });
      assert.equal(focused.ok, true);
      assert.equal(focused.targetId, first.targetId);
    } finally {
      await stopRuntime(runtime);
      await fs.rm(fixture.dir, { recursive: true, force: true });
    }
  }
);

test(
  "browser server contract: omitted targetId sticks to lastTargetId and stale targetId falls back to the only tab",
  { timeout: 120000 },
  async () => {
    const runtime = await startRuntime();
    const fixture = await createContractHtml();
    try {
      const first = await postJson(`${runtime.baseUrl}/tabs/open`, {
        url: `file://${fixture.filePath}#first`
      });
      const second = await postJson(`${runtime.baseUrl}/tabs/open`, {
        url: `file://${fixture.filePath}#second`
      });

      const focusFirst = await postJson(`${runtime.baseUrl}/tabs/focus`, {
        targetId: first.targetId
      });
      assert.equal(focusFirst.targetId, first.targetId);

      const navigateImplicit = await postJson(`${runtime.baseUrl}/navigate`, {
        url: `file://${fixture.filePath}#implicit-last-target`
      });
      assert.equal(
        navigateImplicit.targetId,
        first.targetId,
        "expected omitted targetId to reuse lastTargetId"
      );
      assert.match(navigateImplicit.url, /#implicit-last-target$/);

      const closeSecond = await fetch(`${runtime.baseUrl}/tabs/${encodeURIComponent(second.targetId)}`, {
        method: "DELETE"
      });
      assert.equal(closeSecond.status, 200);

      const staleFallback = await postJson(`${runtime.baseUrl}/tabs/focus`, {
        targetId: "STALE_TARGET"
      });
      assert.equal(
        staleFallback.targetId,
        first.targetId,
        "expected stale targetId to fall back to the only remaining tab"
      );
    } finally {
      await stopRuntime(runtime);
      await fs.rm(fixture.dir, { recursive: true, force: true });
    }
  }
);
