import http from "node:http";
import { randomUUID } from "node:crypto";
import { chromium } from "playwright";
import { installExtensionRelay } from "./relay.mjs";
import { listenWithCompatibleRuntimeProbe } from "./runtime-startup.mjs";
import { resolveTargetIdFromCandidates } from "./target-id.mjs";

const RUNTIME_IDENTITY = "IronClaw/browser-runtime";
const host = process.env.IRONCLAW_BROWSER_RUNTIME_HOST || "127.0.0.1";
const port = Number(process.env.IRONCLAW_BROWSER_RUNTIME_PORT || "24242");
const headless = process.env.IRONCLAW_BROWSER_RUNTIME_HEADLESS !== "0";
const defaultChromeCdpUrl = process.env.IRONCLAW_BROWSER_CHROME_CDP_URL || "http://127.0.0.1:9222";
const chromeMode = process.env.IRONCLAW_BROWSER_CHROME_MODE || "direct";
const relayToken = process.env.IRONCLAW_BROWSER_RELAY_TOKEN || "";
const gatewayToken =
  process.env.IRONCLAW_BROWSER_GATEWAY_TOKEN ||
  process.env.GATEWAY_AUTH_TOKEN ||
  "";

const profiles = new Map();
let reusedRuntimeKeepalive = null;
let runtimeStartup = {
  mode: "starting",
  baseUrl: `http://${host}:${port}`,
  reused: false
};

function boolFromEnv(name, fallback = true) {
  const value = process.env[name];
  if (value === undefined) {
    return fallback;
  }
  return !["0", "false", "False", "FALSE"].includes(value);
}

const evaluateEnabled = boolFromEnv("IRONCLAW_BROWSER_RUNTIME_EVALUATE", true);

function profileNameFromUrl(url) {
  return url.searchParams.get("profile") || "default";
}

async function ensureProfile(name) {
  let profile = profiles.get(name);
  if (profile) {
    return profile;
  }

  let browser;
  let context;
  let driver = "openclaw";
  let remote = false;

  if (name === "chrome") {
    const relayCdpUrl = `http://${host}:${port}`;
    browser = await chromium.connectOverCDP(chromeMode === "relay" ? relayCdpUrl : defaultChromeCdpUrl);
    context = browser.contexts()[0] || (await browser.newContext());
    driver = "chrome";
    remote = true;
  } else {
    browser = await chromium.launch({ headless });
    context = await browser.newContext();
  }

  profile = {
    name,
    browser,
    context,
    driver,
    remote,
    pages: new Map(),
    consoleMessages: new Map(),
    refState: new Map(),
    lastTargetId: null
  };
  profiles.set(name, profile);
  return profile;
}

async function closeProfile(name) {
  const profile = profiles.get(name);
  if (!profile) {
    return false;
  }
  if (!profile.remote || boolFromEnv("IRONCLAW_BROWSER_CHROME_ALLOW_CLOSE", false)) {
    await profile.browser.close();
  }
  profiles.delete(name);
  return true;
}

function json(res, status, body) {
  const payload = JSON.stringify(body);
  res.writeHead(status, {
    "Content-Type": "application/json",
    "Content-Length": Buffer.byteLength(payload)
  });
  res.end(payload);
}

function notFound(res, message = "not found") {
  json(res, 404, { error: message });
}

function badRequest(res, message) {
  json(res, 400, { error: message });
}

function internalError(res, error) {
  const message = error?.message || String(error);
  let status = 500;
  if (message.includes("ambiguous target id prefix")) {
    status = 409;
  } else if (message.includes("tab not found")) {
    status = 404;
  }
  json(res, status, { error: message });
}

async function readJsonBody(req) {
  const chunks = [];
  for await (const chunk of req) {
    chunks.push(chunk);
  }
  if (chunks.length === 0) {
    return {};
  }
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

function normalizeActionRequest(body) {
  if (!body || typeof body !== "object") {
    return {};
  }
  const normalized = { ...body };
  if (!normalized.ref && typeof normalized.element === "string" && normalized.element) {
    normalized.ref = normalized.element;
  }
  return normalized;
}

function attachPageListeners(profile, page, targetId) {
  if (!profile.consoleMessages.has(targetId)) {
    profile.consoleMessages.set(targetId, []);
  }
  page.on("console", (msg) => {
    const entries = profile.consoleMessages.get(targetId) || [];
    entries.push({
      type: msg.type(),
      text: msg.text(),
      location: msg.location()
    });
    if (entries.length > 200) {
      entries.splice(0, entries.length - 200);
    }
    profile.consoleMessages.set(targetId, entries);
  });
  page.on("pageerror", (err) => {
    const entries = profile.consoleMessages.get(targetId) || [];
    entries.push({
      type: "pageerror",
      text: err.message
    });
    if (entries.length > 200) {
      entries.splice(0, entries.length - 200);
    }
    profile.consoleMessages.set(targetId, entries);
  });
  page.on("close", () => {
    profile.pages.delete(targetId);
    profile.refState.delete(targetId);
    if (profile.lastTargetId === targetId) {
      profile.lastTargetId = null;
    }
  });
}

async function registerPage(profile, page) {
  let targetId = page.__ironclawTargetId;
  if (!targetId) {
    targetId = randomUUID();
    page.__ironclawTargetId = targetId;
    attachPageListeners(profile, page, targetId);
  }
  profile.pages.set(targetId, page);
  profile.lastTargetId = targetId;
  return targetId;
}

async function listTabs(profile) {
  for (const page of profile.context.pages()) {
    await registerPage(profile, page);
  }
  const tabs = [];
  for (const [targetId, page] of profile.pages.entries()) {
    if (page.isClosed()) {
      profile.pages.delete(targetId);
      continue;
    }
    tabs.push({
      targetId,
      title: await page.title(),
      url: page.url()
    });
  }
  return tabs;
}

function resolveTargetIdFromProfile(profile, rawTargetId) {
  return resolveTargetIdFromCandidates(rawTargetId, Array.from(profile.pages.keys()));
}

async function resolvePage(profile, targetId) {
  if (targetId) {
    const resolved = resolveTargetIdFromProfile(profile, targetId);
    if (!resolved.ok) {
      if (resolved.reason === "ambiguous") {
        throw new Error("ambiguous target id prefix");
      }
      const candidates = Array.from(profile.pages.entries()).filter(([, page]) => !page.isClosed());
      if (candidates.length === 1) {
        const [fallbackTargetId, fallbackPage] = candidates[0];
        profile.lastTargetId = fallbackTargetId;
        return { page: fallbackPage, targetId: fallbackTargetId };
      }
      throw new Error("tab not found");
    }
    const page = profile.pages.get(resolved.targetId);
    if (!page || page.isClosed()) {
      throw new Error("tab not found");
    }
    profile.lastTargetId = resolved.targetId;
    return { page, targetId: resolved.targetId };
  }

  if (profile.lastTargetId) {
    const page = profile.pages.get(profile.lastTargetId);
    if (page && !page.isClosed()) {
      return { page, targetId: profile.lastTargetId };
    }
  }

  const pages = profile.context.pages().filter((page) => !page.isClosed());
  if (pages.length === 0) {
    throw new Error("No open tabs");
  }
  const page = pages[pages.length - 1];
  const resolvedTargetId = await registerPage(profile, page);
  return { page, targetId: resolvedTargetId };
}

function defaultElementName(el) {
  return (
    (el.innerText || el.textContent || el.getAttribute("aria-label") || el.value || "").trim()
  );
}

function cssEscapeIdent(value) {
  return String(value).replace(/(["\\#.:[\]>+~ ])/g, "\\$1");
}

function buildStableSelector(el) {
  if (el.id) {
    return `#${cssEscapeIdent(el.id)}`;
  }
  const tag = el.tagName.toLowerCase();
  const name = el.getAttribute("name");
  if (name) {
    return `${tag}[name="${String(name).replace(/"/g, '\\"')}"]`;
  }
  const aria = el.getAttribute("aria-label");
  if (aria) {
    return `${tag}[aria-label="${String(aria).replace(/"/g, '\\"')}"]`;
  }
  if (tag === "a" && el.getAttribute("href")) {
    return `a[href="${String(el.getAttribute("href")).replace(/"/g, '\\"')}"]`;
  }

  const segments = [];
  let current = el;
  while (current && current.nodeType === Node.ELEMENT_NODE && segments.length < 6) {
    let segment = current.tagName.toLowerCase();
    if (current.id) {
      segment += `#${cssEscapeIdent(current.id)}`;
      segments.unshift(segment);
      break;
    }
    const siblings = Array.from(current.parentElement?.children || []).filter(
      (node) => node.tagName === current.tagName
    );
    if (siblings.length > 1) {
      const index = siblings.indexOf(current) + 1;
      segment += `:nth-of-type(${index})`;
    }
    segments.unshift(segment);
    current = current.parentElement;
  }
  return segments.join(" > ");
}

function getRoleName(el) {
  return (
    el.getAttribute("aria-label") ||
    el.getAttribute("name") ||
    el.getAttribute("placeholder") ||
    defaultElementName(el)
  ).trim();
}

function roleForElement(tagName) {
  switch (tagName) {
    case "a":
      return "link";
    case "button":
      return "button";
    case "input":
      return "input";
    case "select":
      return "select";
    case "textarea":
      return "textarea";
    default:
      return "element";
  }
}

function roleSignatureForElement(el) {
  const role = roleForElement(el.tagName.toLowerCase());
  const name = getRoleName(el);
  return `${role}::${name}`;
}

function getOrCreateRefState(profile, targetId) {
  let state = profile.refState.get(targetId);
  if (!state) {
    state = {
      mode: "role",
      nextAriaRef: 1,
      selectorToAriaRef: new Map(),
      refToSelector: new Map(),
      refMeta: new Map()
    };
    profile.refState.set(targetId, state);
  }
  return state;
}

async function buildSnapshot(page, options) {
  const {
    targetId,
    format = "ai",
    maxChars = 4000,
    limit = 40,
    selector = null,
    refsMode = "role",
    profile
  } = options;
  const snapshot = await page.evaluate(
    ({ maxChars, limit, selector }) => {
      const root = selector ? document.querySelector(selector) : document.body;
      if (!root) {
        return { url: location.href, title: document.title, text: "", items: [] };
      }

      const items = [];
      const roleCounts = new Map();
      function roleForElement(tagName) {
        switch (tagName) {
          case "a":
            return "link";
          case "button":
            return "button";
          case "input":
            return "input";
          case "select":
            return "select";
          case "textarea":
            return "textarea";
          default:
            return "element";
        }
      }
      function cssEscapeIdent(value) {
        return String(value).replace(/(["\\#.:[\]>+~ ])/g, "\\$1");
      }
      function buildStableSelector(el) {
        if (el.id) {
          return `#${cssEscapeIdent(el.id)}`;
        }
        const tag = el.tagName.toLowerCase();
        const name = el.getAttribute("name");
        if (name) {
          return `${tag}[name="${String(name).replace(/"/g, '\\"')}"]`;
        }
        const aria = el.getAttribute("aria-label");
        if (aria) {
          return `${tag}[aria-label="${String(aria).replace(/"/g, '\\"')}"]`;
        }
        if (tag === "a" && el.getAttribute("href")) {
          return `a[href="${String(el.getAttribute("href")).replace(/"/g, '\\"')}"]`;
        }

        const segments = [];
        let current = el;
        while (current && current.nodeType === Node.ELEMENT_NODE && segments.length < 6) {
          let segment = current.tagName.toLowerCase();
          if (current.id) {
            segment += `#${cssEscapeIdent(current.id)}`;
            segments.unshift(segment);
            break;
          }
          const siblings = Array.from(current.parentElement?.children || []).filter(
            (node) => node.tagName === current.tagName
          );
          if (siblings.length > 1) {
            const index = siblings.indexOf(current) + 1;
            segment += `:nth-of-type(${index})`;
          }
          segments.unshift(segment);
          current = current.parentElement;
        }
        return segments.join(" > ");
      }
      const elements = root.querySelectorAll(
        "a,button,input,textarea,select,[role='button'],[role='link'],[tabindex]"
      );
      for (const el of elements) {
        if (items.length >= limit) {
          break;
        }
        const text = (el.innerText || el.getAttribute("aria-label") || el.value || "").trim();
        const role = roleForElement(el.tagName.toLowerCase());
        const name =
          text || el.getAttribute("name") || el.getAttribute("placeholder") || "";
        const roleSignature = `${role}::${name}`;
        const roleIndex = roleCounts.get(roleSignature) || 0;
        roleCounts.set(roleSignature, roleIndex + 1);
        items.push({
          role,
          name,
          roleSignature,
          roleIndex,
          stableSelector: buildStableSelector(el)
        });
      }

      const text = (root.innerText || "").replace(/\s+\n/g, "\n").trim().slice(0, maxChars);
      return {
        url: location.href,
        title: document.title,
        text,
        items
      };
    },
    { maxChars, limit, selector }
  );

  const refState = getOrCreateRefState(profile, targetId);
  refState.mode = refsMode;
  refState.refToSelector.clear();
  refState.refMeta.clear();

  const assignedItems = snapshot.items.map((item, index) => {
    let ref;
    if (refsMode === "aria") {
      ref = refState.selectorToAriaRef.get(item.stableSelector);
      if (!ref) {
        ref = `e${refState.nextAriaRef++}`;
        refState.selectorToAriaRef.set(item.stableSelector, ref);
      }
    } else {
      ref = `e${index + 1}`;
    }
    refState.refToSelector.set(ref, item.stableSelector);
    refState.refMeta.set(ref, {
      selector: item.stableSelector,
      role: item.role,
      name: item.name,
      roleSignature: item.roleSignature,
      roleIndex: item.roleIndex
    });
    return {
      ...item,
      ref,
      selector: item.stableSelector
    };
  });

  if (format === "aria") {
    return {
      ok: true,
      format: "aria",
      targetId,
      url: snapshot.url,
      nodes: assignedItems.map((item) => ({
        ref: item.ref,
        role: item.role,
        name: item.name,
        depth: 0
      })),
      refsMode
    };
  }

  const lines = [
    `Title: ${snapshot.title}`,
    `URL: ${snapshot.url}`,
    "",
    snapshot.text
  ];
  if (assignedItems.length > 0) {
    lines.push("", "Interactive elements:");
    for (const item of assignedItems) {
      const quotedName = item.name ? ` "${item.name}"` : "";
      const nthSuffix = refsMode === "role" && item.roleIndex > 0 ? ` [nth=${item.roleIndex}]` : "";
      lines.push(`- ${item.role}${quotedName} [ref=${item.ref}]${nthSuffix}`.trim());
    }
  }

  const refs = Object.fromEntries(
    assignedItems.map((item) => [
      item.ref,
      {
        role: item.role,
        name: item.name,
        selector: item.selector
      }
    ])
  );

  return {
    ok: true,
    format: "ai",
    refsMode,
    targetId,
    url: snapshot.url,
    snapshot: lines.join("\n").trim(),
    refs,
    stats: {
      lines: lines.length,
      chars: lines.join("\n").length,
      refs: snapshot.items.length,
      interactive: snapshot.items.length
    }
  };
}

async function resolveActSelector(page, request) {
  const normalized = normalizeActionRequest(request);
  if (normalized.ref) {
    const targetId = page.__ironclawTargetId;
    const profile = Array.from(profiles.values()).find((candidate) => candidate.pages.get(targetId) === page);
    const refState = profile?.refState.get(targetId);
    const selector = refState?.refToSelector.get(normalized.ref);
    if (selector) {
      return selector;
    }
    throw new Error(`Unknown ref "${normalized.ref}". Run a new snapshot and use a ref from that snapshot.`);
  }
  if (normalized.selector) {
    return normalized.selector;
  }
  throw new Error("act requires ref or selector");
}

async function resolveElementLocator(page, request) {
  return page.locator(await resolveActSelector(page, request));
}

async function resolveUploadLocator(page, request) {
  const normalized = normalizeActionRequest(request);
  if (normalized.inputRef) {
    const targetId = page.__ironclawTargetId;
    const profile = Array.from(profiles.values()).find((candidate) => candidate.pages.get(targetId) === page);
    const selector = profile?.refState.get(targetId)?.refToSelector.get(normalized.inputRef);
    if (selector) {
      return page.locator(selector);
    }
    throw new Error(`Unknown inputRef "${normalized.inputRef}". Run a new snapshot and use a ref from that snapshot.`);
  }
  return resolveElementLocator(page, normalized);
}

async function performWait(page, body) {
  const timeoutMs = body.timeoutMs || 20000;

  if (body.timeMs !== undefined) {
    await page.waitForTimeout(body.timeMs);
  }
  if (body.text) {
    await page.getByText(body.text).waitFor({ timeout: timeoutMs });
  }
  if (body.textGone) {
    await page.getByText(body.textGone).waitFor({ state: "hidden", timeout: timeoutMs });
  }
  if (body.selector) {
    await page.locator(body.selector).waitFor({ timeout: timeoutMs });
  }
  if (body.url) {
    await page.waitForURL(body.url, { timeout: timeoutMs });
  }
  if (body.loadState) {
    await page.waitForLoadState(body.loadState, { timeout: timeoutMs });
  }
  if (body.fn) {
    if (!evaluateEnabled) {
      throw new Error("act:evaluate and wait:fn are disabled by IRONCLAW_BROWSER_RUNTIME_EVALUATE");
    }
    await page.waitForFunction(
      ({ source }) => {
        const fn = (0, eval)(`(${source})`);
        return fn();
      },
      { source: body.fn },
      { timeout: timeoutMs }
    );
  }
}

function fieldDescriptor(field) {
  return field?.ref || field?.selector || field?.label || field?.name || field?.placeholder || "field";
}

function selectLocatorForField(page, field) {
  if (field.ref) {
    return page.locator(`[data-ironclaw-ref="${field.ref}"]`);
  }
  if (field.selector) {
    return page.locator(field.selector);
  }
  if (field.label) {
    return page.getByLabel(field.label, { exact: false });
  }
  if (field.placeholder) {
    return page.getByPlaceholder(field.placeholder, { exact: false });
  }
  if (field.name) {
    return page.locator(`[name="${field.name}"]`);
  }
  throw new Error(`fill field is missing locator metadata for ${fieldDescriptor(field)}`);
}

async function fillField(page, field, timeoutMs) {
  const locator = selectLocatorForField(page, field);
  const values = Array.isArray(field.values) ? field.values.filter((v) => typeof v === "string") : [];
  const value = field.value ?? field.text ?? (values.length > 0 ? values[0] : "");
  const tagName = await locator.evaluate((el) => el.tagName.toLowerCase());
  const inputType = await locator.evaluate((el) => (el instanceof HTMLInputElement ? el.type : ""));

  if (tagName === "select") {
    const optionValues = values.length > 0 ? values : [String(value)];
    await locator.selectOption(optionValues, { timeout: timeoutMs });
    return;
  }

  if (inputType === "checkbox" || inputType === "radio") {
    const shouldCheck = typeof field.checked === "boolean" ? field.checked : Boolean(value);
    if (shouldCheck) {
      await locator.check({ timeout: timeoutMs });
    } else {
      await locator.uncheck({ timeout: timeoutMs });
    }
    return;
  }

  await locator.fill(String(value ?? ""), { timeout: timeoutMs });
}

async function handleAct(profile, reqUrl, req, res) {
  const body = normalizeActionRequest(await readJsonBody(req));
  const { page, targetId } = await resolvePage(profile, body.targetId);
  const kind = body.kind;
  switch (kind) {
    case "click": {
      const locator = await resolveElementLocator(page, body);
      if (body.doubleClick) {
        await locator.dblclick({
          button: body.button || "left",
          timeout: body.timeoutMs || 20000
        });
      } else {
        await locator.click({
          button: body.button || "left",
          timeout: body.timeoutMs || 20000
        });
      }
      return json(res, 200, { ok: true, targetId });
    }
    case "type": {
      const locator = await resolveElementLocator(page, body);
      await locator.fill("", { timeout: body.timeoutMs || 20000 });
      await locator.type(body.text || "", {
        delay: body.slowly ? 80 : 0,
        timeout: body.timeoutMs || 20000
      });
      if (body.submit) {
        await page.keyboard.press("Enter");
      }
      return json(res, 200, { ok: true, targetId });
    }
    case "press": {
      await page.keyboard.press(body.key || "Enter", {
        delay: body.delayMs || 0
      });
      return json(res, 200, { ok: true, targetId });
    }
    case "hover": {
      const locator = await resolveElementLocator(page, body);
      await locator.hover({ timeout: body.timeoutMs || 20000 });
      return json(res, 200, { ok: true, targetId });
    }
    case "drag": {
      if (!body.startRef || !body.endRef) {
        return badRequest(res, "startRef and endRef are required");
      }
      const from = page.locator(`[data-ironclaw-ref="${body.startRef}"]`);
      const to = page.locator(`[data-ironclaw-ref="${body.endRef}"]`);
      await from.dragTo(to, { timeout: body.timeoutMs || 20000 });
      return json(res, 200, { ok: true, targetId });
    }
    case "select": {
      const locator = await resolveElementLocator(page, body);
      const values = Array.isArray(body.values) ? body.values.filter((v) => typeof v === "string") : [];
      if (values.length === 0) {
        return badRequest(res, "values are required");
      }
      await locator.selectOption(values, { timeout: body.timeoutMs || 20000 });
      return json(res, 200, { ok: true, targetId });
    }
    case "fill": {
      const fields = Array.isArray(body.fields) ? body.fields.filter((v) => v && typeof v === "object") : [];
      if (fields.length === 0) {
        return badRequest(res, "fields are required");
      }
      const timeoutMs = body.timeoutMs || 20000;
      for (const field of fields) {
        await fillField(page, field, timeoutMs);
      }
      return json(res, 200, { ok: true, targetId });
    }
    case "resize": {
      if (!body.width || !body.height) {
        return badRequest(res, "width and height are required");
      }
      await page.setViewportSize({ width: body.width, height: body.height });
      return json(res, 200, {
        ok: true,
        targetId,
        viewport: { width: body.width, height: body.height }
      });
    }
    case "wait": {
      if (
        body.timeMs === undefined &&
        !body.text &&
        !body.textGone &&
        !body.selector &&
        !body.url &&
        !body.loadState &&
        !body.fn
      ) {
        return badRequest(
          res,
          "wait requires at least one of: timeMs, text, textGone, selector, url, loadState, fn"
        );
      }
      await performWait(page, body);
      return json(res, 200, { ok: true, targetId });
    }
    case "evaluate": {
      if (!evaluateEnabled) {
        return badRequest(res, "act:evaluate is disabled by IRONCLAW_BROWSER_RUNTIME_EVALUATE");
      }
      if (!body.fn) {
        return badRequest(res, "fn is required");
      }
      let result;
      if (body.ref || body.selector || body.element) {
        const locator = await resolveElementLocator(page, body);
        result = await locator.evaluate((el, source) => {
          const fn = (0, eval)(`(${source})`);
          return fn(el);
        }, body.fn);
      } else {
        result = await page.evaluate((source) => {
          const fn = (0, eval)(`(${source})`);
          return fn();
        }, body.fn);
      }
      return json(res, 200, { ok: true, targetId, url: page.url(), result });
    }
    case "close": {
      await page.close({ runBeforeUnload: false });
      profile.pages.delete(targetId);
      return json(res, 200, { ok: true, targetId });
    }
    default:
      return badRequest(res, `Unsupported act kind '${kind}'`);
  }
}

async function armDialogHook(profile, body) {
  const { page, targetId } = await resolvePage(profile, body.targetId);
  const accept = body.accept !== false;
  const timeoutMs = body.timeoutMs || 20000;

  page
    .waitForEvent("dialog", { timeout: timeoutMs })
    .then(async (dialog) => {
      if (accept) {
        await dialog.accept(body.promptText);
      } else {
        await dialog.dismiss();
      }
    })
    .catch(() => {});

  return { ok: true, armed: true, targetId };
}

async function armFileChooserHook(profile, body) {
  const normalized = normalizeActionRequest(body);
  const { page, targetId } = await resolvePage(profile, normalized.targetId);
  const paths = Array.isArray(normalized.paths)
    ? normalized.paths.filter((v) => typeof v === "string" && v)
    : [];
  if (paths.length === 0) {
    throw new Error("paths are required");
  }

  if (normalized.inputRef || normalized.element) {
    const locator = await resolveUploadLocator(page, normalized);
    await locator.setInputFiles(paths, { timeout: normalized.timeoutMs || 20000 });
    return { ok: true, armed: false, targetId };
  }

  const waitForChooser = page.waitForEvent("filechooser", {
    timeout: normalized.timeoutMs || 20000
  });
  if (normalized.ref) {
    const locator = await resolveElementLocator(page, normalized);
    await locator.click({ timeout: normalized.timeoutMs || 20000 });
  }
  const chooser = await waitForChooser;
  await chooser.setFiles(paths);
  return { ok: true, armed: true, targetId };
}

const relay = installExtensionRelay(undefined, {
  baseUrl: `http://${host}:${port}`,
  host,
  port,
  relayToken,
  gatewayToken
});

const server = http.createServer(async (req, res) => {
  try {
    const reqUrl = new URL(req.url || "/", `http://${host}:${port}`);
    const profileName = profileNameFromUrl(reqUrl);
    const profile = profiles.get(profileName);

    if (req.method === "GET" && reqUrl.pathname === "/_ironclaw/runtime") {
      const tabs = profile ? await listTabs(profile) : [];
      return json(res, 200, {
        identity: RUNTIME_IDENTITY,
        relayIdentity: relay.relayInfo.browserIdentity,
        startup: runtimeStartup,
        relayConnected: relay.relayInfo.extensionConnected(),
        enabled: true,
        running: Boolean(profile),
        profile: profileName,
        driver: profile?.driver || (profileName === "chrome" ? "chrome" : "openclaw"),
        remote: profile?.remote || false,
        chromeMode: profileName === "chrome" ? chromeMode : undefined,
        headless,
        tabCount: tabs.length
      });
    }

    if (relay.handleHttp(req, res)) {
      return;
    }

    if (req.method === "GET" && reqUrl.pathname === "/") {
      const tabs = profile ? await listTabs(profile) : [];
      return json(res, 200, {
        identity: RUNTIME_IDENTITY,
        enabled: true,
        running: Boolean(profile),
        profile: profileName,
        driver: profile?.driver || (profileName === "chrome" ? "chrome" : "openclaw"),
        remote: profile?.remote || false,
        chromeMode: profileName === "chrome" ? chromeMode : undefined,
        relayConnected: relay.relayInfo.extensionConnected(),
        headless,
        tabCount: tabs.length
      });
    }

    if (req.method === "POST" && reqUrl.pathname === "/start") {
      await ensureProfile(profileName);
      return json(res, 200, { ok: true, profile: profileName });
    }

    if (req.method === "POST" && reqUrl.pathname === "/stop") {
      const closed = await closeProfile(profileName);
      return json(res, 200, { ok: true, profile: profileName, closed });
    }

    if (req.method === "GET" && reqUrl.pathname === "/profiles") {
      const items = await Promise.all(
        Array.from(profiles.values()).map(async (profile) => ({
          name: profile.name,
          driver: profile.driver,
          remote: profile.remote,
          running: true,
          tabCount: (await listTabs(profile)).length,
          isDefault: profile.name === "default"
        }))
      );
      return json(res, 200, { profiles: items });
    }

    if (req.method === "GET" && reqUrl.pathname === "/tabs") {
      const profile = await ensureProfile(profileName);
      return json(res, 200, { running: true, tabs: await listTabs(profile) });
    }

    if (req.method === "POST" && reqUrl.pathname === "/tabs/open") {
      const profile = await ensureProfile(profileName);
      const body = await readJsonBody(req);
      if (!body.url) {
        return badRequest(res, "url is required");
      }
      const page = await profile.context.newPage();
      await page.goto(body.url, { waitUntil: "domcontentloaded" });
      const targetId = await registerPage(profile, page);
      return json(res, 200, {
        targetId,
        title: await page.title(),
        url: page.url()
      });
    }

    if (req.method === "POST" && reqUrl.pathname === "/tabs/focus") {
      const profile = await ensureProfile(profileName);
      const body = await readJsonBody(req);
      const { page, targetId } = await resolvePage(profile, body.targetId);
      await page.bringToFront();
      return json(res, 200, { ok: true, targetId });
    }

    if (req.method === "DELETE" && reqUrl.pathname.startsWith("/tabs/")) {
      const profile = await ensureProfile(profileName);
      const targetId = decodeURIComponent(reqUrl.pathname.slice("/tabs/".length));
      const { page } = await resolvePage(profile, targetId);
      await page.close();
      profile.pages.delete(targetId);
      profile.refState.delete(targetId);
      if (profile.lastTargetId === targetId) {
        profile.lastTargetId = null;
      }
      return json(res, 200, { ok: true, targetId });
    }

    if (req.method === "POST" && reqUrl.pathname === "/navigate") {
      const profile = await ensureProfile(profileName);
      const body = await readJsonBody(req);
      if (!body.url) {
        return badRequest(res, "url is required");
      }
      const { page, targetId } = await resolvePage(profile, body.targetId);
      await page.goto(body.url, { waitUntil: "domcontentloaded" });
      return json(res, 200, {
        ok: true,
        targetId,
        url: page.url(),
        title: await page.title()
      });
    }

    if (req.method === "GET" && reqUrl.pathname === "/snapshot") {
      const profile = await ensureProfile(profileName);
      const { page, targetId } = await resolvePage(profile, reqUrl.searchParams.get("targetId"));
      const format = reqUrl.searchParams.get("format") || "ai";
      const maxChars = Number(reqUrl.searchParams.get("maxChars") || "4000");
      const limit = Number(reqUrl.searchParams.get("limit") || "40");
      const selector = reqUrl.searchParams.get("selector");
      const refsMode = reqUrl.searchParams.get("refs") === "aria" ? "aria" : "role";
      return json(
        res,
        200,
        await buildSnapshot(page, { targetId, format, maxChars, limit, selector, refsMode, profile })
      );
    }

    if (req.method === "POST" && reqUrl.pathname === "/screenshot") {
      const profile = await ensureProfile(profileName);
      const body = normalizeActionRequest(await readJsonBody(req));
      const { page, targetId } = await resolvePage(profile, body.targetId);
      const type = body.type === "jpeg" ? "jpeg" : "png";
      const screenshotOptions = {
        type,
        fullPage: Boolean(body.fullPage)
      };
      const buffer =
        body.ref || body.selector || body.element
          ? await (await resolveElementLocator(page, body)).screenshot(screenshotOptions)
          : await page.screenshot(screenshotOptions);
      res.writeHead(200, {
        "Content-Type": type === "jpeg" ? "image/jpeg" : "image/png",
        "Content-Length": buffer.length,
        "X-Browser-Target-Id": targetId,
        "X-Browser-Url": page.url()
      });
      res.end(buffer);
      return;
    }

    if (req.method === "POST" && reqUrl.pathname === "/pdf") {
      const profile = await ensureProfile(profileName);
      const body = await readJsonBody(req);
      const { page, targetId } = await resolvePage(profile, body.targetId);
      const buffer = await page.pdf({ format: "A4", printBackground: true });
      res.writeHead(200, {
        "Content-Type": "application/pdf",
        "Content-Length": buffer.length,
        "X-Browser-Target-Id": targetId,
        "X-Browser-Url": page.url()
      });
      res.end(buffer);
      return;
    }

    if (req.method === "GET" && reqUrl.pathname === "/console") {
      const profile = await ensureProfile(profileName);
      const { targetId } = await resolvePage(profile, reqUrl.searchParams.get("targetId"));
      return json(res, 200, {
        ok: true,
        targetId,
        messages: profile.consoleMessages.get(targetId) || []
      });
    }

    if (req.method === "POST" && reqUrl.pathname === "/act") {
      const profile = await ensureProfile(profileName);
      return await handleAct(profile, reqUrl, req, res);
    }

    if (req.method === "POST" && reqUrl.pathname === "/hooks/dialog") {
      const profile = await ensureProfile(profileName);
      const body = normalizeActionRequest(await readJsonBody(req));
      return json(res, 200, await armDialogHook(profile, body));
    }

    if (req.method === "POST" && reqUrl.pathname === "/hooks/file-chooser") {
      const profile = await ensureProfile(profileName);
      const body = normalizeActionRequest(await readJsonBody(req));
      return json(res, 200, await armFileChooserHook(profile, body));
    }

    return notFound(res);
  } catch (error) {
    return internalError(res, error);
  }
});

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, async () => {
    if (reusedRuntimeKeepalive) {
      clearInterval(reusedRuntimeKeepalive);
      reusedRuntimeKeepalive = null;
    }
    for (const name of Array.from(profiles.keys())) {
      await closeProfile(name);
    }
    if (server.listening) {
      server.close(() => process.exit(0));
      return;
    }
    process.exit(0);
  });
}

server.on("upgrade", (req, socket, head) => {
  if (relay.handleUpgrade(req, socket, head)) {
    return;
  }
  socket.destroy();
});

const startup = await listenWithCompatibleRuntimeProbe({
  server,
  host,
  port,
  relayToken,
  gatewayToken,
  onReuse: async ({ baseUrl }) => {
    console.log(`ironclaw browser runtime reused existing compatible runtime at ${baseUrl}`);
    reusedRuntimeKeepalive = setInterval(() => {}, 60 * 60 * 1000);
  }
});
runtimeStartup = {
  mode: startup.mode,
  baseUrl: startup.baseUrl,
  reused: Boolean(startup.reused)
};

if (!startup.reused) {
  console.log(`ironclaw browser runtime listening on http://${host}:${port}`);
  console.log(`ironclaw browser relay auth header: ${relay.relayInfo.relayAuthHeader}`);
  if (relay.relayInfo.relayToken) {
    console.log(`ironclaw browser relay explicit token enabled`);
  } else if (gatewayToken.trim()) {
    console.log(`ironclaw browser relay gateway-token compatibility enabled`);
  } else {
    console.log(`ironclaw browser relay auth disabled`);
  }
} else {
  console.log(`ironclaw browser relay auth header: ${relay.relayInfo.relayAuthHeader}`);
  console.log(`ironclaw browser runtime is attached to an already-running compatible instance`);
}
