import {
  buildRelayWsUrl,
  deriveRelayToken,
  isRetryableReconnectError,
  reconnectDelayMs
} from "./background-utils.js";

const DEFAULT_RELAY_URL = "http://127.0.0.1:24242";
const DEBUGGER_VERSION = "1.3";

let relaySocket = null;
let relayUrlInUse = "";
let relayTokenInUse = "";
let relayGatewayTokenInUse = "";
let relayOpenPromise = null;
let reconnectAttempt = 0;
let reconnectTimer = null;
let nextSession = 1;

const pendingCommands = new Map();
const attachedTabs = new Map();
const reattachPending = new Set();

function tabSessionId(tabId) {
  const current = nextSession++;
  return `cb-tab-${current}`;
}

async function getRelayConfig() {
  const stored = await chrome.storage.local.get(["relayUrl", "relayToken", "gatewayToken"]);
  return {
    relayUrl: String(stored.relayUrl || DEFAULT_RELAY_URL),
    relayToken: String(stored.relayToken || ""),
    gatewayToken: String(stored.gatewayToken || stored.relayToken || "")
  };
}

function withTokenHeader(headers, relayToken) {
  if (relayToken) {
    headers["x-ironclaw-relay-token"] = relayToken;
  }
  return headers;
}

async function checkRelayReachable(relayUrl, relayToken) {
  const res = await fetch(new URL("/", relayUrl), {
    method: "HEAD",
    headers: withTokenHeader({}, relayToken),
    signal: AbortSignal.timeout(2000)
  });
  if (!res.ok) {
    throw new Error(`relay status returned ${res.status}`);
  }
}

async function setActionState(tabId, kind, message) {
  if (!tabId) {
    return;
  }

  let badgeText = "";
  let badgeColor = "#64748b";
  let title = "IronClaw Browser Relay";

  switch (kind) {
    case "attached":
      badgeText = "ON";
      badgeColor = "#15803d";
      title = message || "Attached to IronClaw";
      break;
    case "pending":
      badgeText = "...";
      badgeColor = "#0f766e";
      title = message || "Connecting to IronClaw relay";
      break;
    case "error":
      badgeText = "ERR";
      badgeColor = "#b91c1c";
      title = message || "Relay error";
      break;
    default:
      title = message || "Attach current tab to IronClaw";
      break;
  }

  await chrome.action.setBadgeBackgroundColor({ tabId, color: badgeColor });
  await chrome.action.setBadgeText({ tabId, text: badgeText });
  await chrome.action.setTitle({ tabId, title });
}

function sendToRelay(payload) {
  const socket = relaySocket;
  if (!socket || socket.readyState !== WebSocket.OPEN) {
    throw new Error("relay websocket is not connected");
  }
  socket.send(JSON.stringify(payload));
}

async function sendRelayEvent(method, params, sessionId) {
  sendToRelay({
    method: "forwardCDPEvent",
    params: {
      method,
      params,
      sessionId
    }
  });
}

async function persistState() {
  try {
    const persistedTabs = [];
    for (const [tabId, entry] of attachedTabs.entries()) {
      if (!entry?.sessionId || !entry?.targetInfo?.targetId) {
        continue;
      }
      persistedTabs.push({
        tabId,
        sessionId: entry.sessionId,
        targetInfo: entry.targetInfo
      });
    }
    await chrome.storage.session.set({
      persistedTabs,
      nextSession
    });
  } catch {
    // Ignore environments without session storage.
  }
}

async function rehydrateState() {
  try {
    const stored = await chrome.storage.session.get(["persistedTabs", "nextSession"]);
    if (Number.isFinite(stored.nextSession)) {
      nextSession = Math.max(nextSession, stored.nextSession);
    }

    for (const entry of stored.persistedTabs || []) {
      if (typeof entry?.tabId !== "number" || !entry?.sessionId || !entry?.targetInfo?.targetId) {
        continue;
      }
      attachedTabs.set(entry.tabId, {
        tabId: entry.tabId,
        sessionId: entry.sessionId,
        targetInfo: entry.targetInfo
      });
      await setActionState(entry.tabId, "attached", "Attached to IronClaw");
    }

    for (const [tabId, entry] of Array.from(attachedTabs.entries())) {
      try {
        await chrome.tabs.get(tabId);
        await chrome.debugger.sendCommand({ tabId }, "Runtime.evaluate", {
          expression: "1",
          returnByValue: true
        });
      } catch {
        attachedTabs.delete(tabId);
        await setActionState(tabId, "", "Attach current tab to IronClaw");
      }
    }

    if (attachedTabs.size > 0) {
      try {
        await ensureRelaySocket();
        await reannounceAttachedTabs();
      } catch {
        scheduleReconnect();
      }
    }
  } catch {
    // Ignore restore errors; user can reattach manually.
  }
}

function cancelReconnect() {
  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  reconnectAttempt = 0;
}

async function closeRelaySocket(reason, opts = {}) {
  const socket = relaySocket;
  relaySocket = null;
  relayOpenPromise = null;

  if (opts.clearConfig !== false) {
    relayUrlInUse = "";
    relayTokenInUse = "";
    relayGatewayTokenInUse = "";
  }

  for (const pending of pendingCommands.values()) {
    pending.reject(new Error(reason || "relay connection closed"));
  }
  pendingCommands.clear();

  if (socket && (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CONNECTING)) {
    socket.close(1000, reason || "closing");
  }
}

async function reannounceAttachedTabs() {
  for (const [tabId, entry] of attachedTabs.entries()) {
    if (!entry?.sessionId || !entry?.targetInfo?.targetId) {
      continue;
    }

    try {
      const tab = await chrome.tabs.get(tabId);
      entry.targetInfo = {
        ...entry.targetInfo,
        title: tab.title || entry.targetInfo.title || "",
        url: tab.url || entry.targetInfo.url || ""
      };
    } catch {
      attachedTabs.delete(tabId);
      await setActionState(tabId, "", "Attach current tab to IronClaw");
      continue;
    }

    try {
      await chrome.debugger.sendCommand({ tabId }, "Runtime.evaluate", {
        expression: "1",
        returnByValue: true
      });
    } catch {
      attachedTabs.delete(tabId);
      await setActionState(tabId, "", "Attach current tab to IronClaw");
      continue;
    }

    try {
      await sendRelayEvent(
        "Target.attachedToTarget",
        {
          sessionId: entry.sessionId,
          targetInfo: { ...entry.targetInfo, attached: true },
          waitingForDebugger: false
        },
        entry.sessionId
      );
      await setActionState(tabId, "attached", "Attached to IronClaw");
    } catch {
      await setActionState(tabId, "pending", "Relay reconnecting…");
    }
  }
  await persistState();
}

function scheduleReconnect() {
  if (reconnectTimer || attachedTabs.size === 0) {
    return;
  }

  const delay = reconnectDelayMs(reconnectAttempt);
  reconnectAttempt += 1;

  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    void (async () => {
      try {
        await ensureRelaySocket();
        reconnectAttempt = 0;
        await reannounceAttachedTabs();
      } catch (error) {
        if (!isRetryableReconnectError(error)) {
          for (const tabId of attachedTabs.keys()) {
            await setActionState(
              tabId,
              "error",
              error instanceof Error ? error.message : String(error)
            );
          }
          return;
        }
        scheduleReconnect();
      }
    })();
  }, delay);
}

async function handleRelayDisconnect(reason) {
  await closeRelaySocket(reason, { clearConfig: false });

  for (const tabId of attachedTabs.keys()) {
    await setActionState(tabId, "pending", "Relay reconnecting…");
  }

  scheduleReconnect();
}

async function onRelayMessage(text) {
  let parsed = null;
  try {
    parsed = JSON.parse(text);
  } catch {
    return;
  }

  if (parsed?.method === "ping") {
    try {
      sendToRelay({ method: "pong" });
    } catch {
      // ignore
    }
    return;
  }

  if (typeof parsed?.id === "number" && (parsed.result !== undefined || parsed.error !== undefined)) {
    const pending = pendingCommands.get(parsed.id);
    if (!pending) {
      return;
    }
    pendingCommands.delete(parsed.id);
    if (parsed.error) {
      pending.reject(new Error(String(parsed.error)));
    } else {
      pending.resolve(parsed.result);
    }
    return;
  }

  if (!parsed || typeof parsed.id !== "number" || parsed.method !== "forwardCDPCommand") {
    return;
  }

  const { method, params, sessionId } = parsed.params || {};
  const tabEntry =
    (sessionId &&
      Array.from(attachedTabs.values()).find((entry) => entry.sessionId === sessionId)) ||
    (typeof params?.targetId === "string" &&
      Array.from(attachedTabs.values()).find(
        (entry) => entry.targetInfo?.targetId === params.targetId
      )) ||
    attachedTabs.values().next().value;

  if (!tabEntry) {
    sendToRelay({ id: parsed.id, error: "no attached tab available" });
    return;
  }

  try {
    let result;
    if (method === "Target.closeTarget") {
      const closeEntry =
        (typeof params?.targetId === "string" &&
          Array.from(attachedTabs.values()).find(
            (entry) => entry.targetInfo?.targetId === params.targetId
          )) ||
        tabEntry;
      await chrome.tabs.remove(closeEntry.tabId);
      result = { success: true };
    } else if (method === "Target.activateTarget") {
      const activateEntry =
        (typeof params?.targetId === "string" &&
          Array.from(attachedTabs.values()).find(
            (entry) => entry.targetInfo?.targetId === params.targetId
          )) ||
        tabEntry;
      const tab = await chrome.tabs.get(activateEntry.tabId).catch(() => null);
      if (tab?.windowId) {
        await chrome.windows.update(tab.windowId, { focused: true }).catch(() => {});
      }
      if (tab) {
        await chrome.tabs.update(activateEntry.tabId, { active: true }).catch(() => {});
      }
      result = {};
    } else {
      const debuggee =
        sessionId && sessionId !== tabEntry.sessionId
          ? { tabId: tabEntry.tabId, sessionId }
          : { tabId: tabEntry.tabId };
      result = await chrome.debugger.sendCommand(debuggee, method, params || {});
    }

    sendToRelay({ id: parsed.id, result: result || {} });
  } catch (error) {
    sendToRelay({
      id: parsed.id,
      error: error instanceof Error ? error.message : String(error)
    });
  }
}

async function ensureRelaySocket() {
  const { relayUrl, relayToken, gatewayToken } = await getRelayConfig();
  const effectiveRelayToken =
    String(relayToken || "").trim() ||
    (gatewayToken.trim()
      ? await deriveRelayToken(gatewayToken, Number(new URL(relayUrl).port || "80"))
      : "");

  if (
    relaySocket &&
    relaySocket.readyState === WebSocket.OPEN &&
    relayUrlInUse === relayUrl &&
    relayTokenInUse === effectiveRelayToken
  ) {
    return relaySocket;
  }

  if (relayOpenPromise) {
    return await relayOpenPromise;
  }

  relayOpenPromise = (async () => {
    await checkRelayReachable(relayUrl, effectiveRelayToken);

    const socket = new WebSocket(await buildRelayWsUrl(relayUrl, gatewayToken, relayToken));
    relayUrlInUse = relayUrl;
    relayTokenInUse = effectiveRelayToken;
    relayGatewayTokenInUse = gatewayToken;

    socket.addEventListener("message", (event) => {
      if (socket !== relaySocket) {
        return;
      }
      void onRelayMessage(String(event.data || ""));
    });

    socket.addEventListener("close", () => {
      if (relaySocket === socket) {
        void handleRelayDisconnect("relay websocket closed");
      }
    });

    socket.addEventListener("error", () => {
      if (relaySocket === socket) {
        void handleRelayDisconnect("relay websocket error");
      }
    });

    await new Promise((resolve, reject) => {
      const onOpen = () => {
        socket.removeEventListener("error", onError);
        resolve();
      };
      const onError = () => {
        socket.removeEventListener("open", onOpen);
        reject(new Error("failed to open relay websocket"));
      };
      socket.addEventListener("open", onOpen, { once: true });
      socket.addEventListener("error", onError, { once: true });
    });

    relaySocket = socket;
    return socket;
  })();

  try {
    const socket = await relayOpenPromise;
    cancelReconnect();
    return socket;
  } finally {
    relayOpenPromise = null;
  }
}

async function attachTab(tab, opts = {}) {
  if (!tab.id) {
    throw new Error("active tab has no id");
  }

  await setActionState(tab.id, "pending", "Connecting to IronClaw relay");
  await ensureRelaySocket();

  try {
    await chrome.debugger.attach({ tabId: tab.id }, DEBUGGER_VERSION);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (!message.toLowerCase().includes("already attached")) {
      throw error;
    }
  }
  const sessionId = tabSessionId(tab.id);
  const targetInfo = {
    targetId: `target-${tab.id}`,
    type: "page",
    title: tab.title || "",
    url: tab.url || ""
  };

  attachedTabs.set(tab.id, {
    tabId: tab.id,
    sessionId,
    targetInfo
  });

  if (!opts.skipAttachedEvent) {
    await sendRelayEvent(
      "Target.attachedToTarget",
      {
        sessionId,
        targetInfo,
        waitingForDebugger: false
      },
      sessionId
    );
  }

  await setActionState(tab.id, "attached", "Attached to IronClaw");
  await persistState();
}

async function detachTab(tabId, reason) {
  const entry = attachedTabs.get(tabId);
  if (!entry) {
    await setActionState(tabId, "", "Attach current tab to IronClaw");
    return;
  }

  try {
    await sendRelayEvent(
      "Target.detachedFromTarget",
      {
        sessionId: entry.sessionId,
        targetId: entry.targetInfo.targetId
      },
      entry.sessionId
    );
  } catch {
    // relay may already be gone
  }

  try {
    await chrome.debugger.detach({ tabId });
  } catch {
    // ignore
  }

  attachedTabs.delete(tabId);
  await setActionState(tabId, "", reason || "Attach current tab to IronClaw");
  await persistState();
}

async function getCurrentTab() {
  const tabs = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
  return tabs[0] || null;
}

async function resolveMessageTab(message) {
  if (typeof message?.tabId === "number") {
    const tab = await chrome.tabs.get(message.tabId).catch(() => null);
    return tab || null;
  }
  if (typeof message?.url === "string" && message.url) {
    const tabs = await chrome.tabs.query({ url: message.url });
    return tabs[0] || null;
  }
  return await getCurrentTab();
}

chrome.action.onClicked.addListener((tab) => {
  void (async () => {
    if (!tab.id) {
      return;
    }
    if (reattachPending.has(tab.id)) {
      reattachPending.delete(tab.id);
      await setActionState(tab.id, "", "Attach current tab to IronClaw");
      return;
    }
    if (attachedTabs.has(tab.id)) {
      await detachTab(tab.id, "Detached from IronClaw");
      return;
    }
    try {
      cancelReconnect();
      await attachTab(tab);
    } catch (error) {
      await setActionState(
        tab.id,
        "error",
        error instanceof Error ? error.message : String(error)
      );
    }
  })();
});

chrome.debugger.onEvent.addListener((source, method, params) => {
  void (async () => {
    if (!source.tabId || !attachedTabs.has(source.tabId)) {
      return;
    }
    const entry = attachedTabs.get(source.tabId);
    if (!entry) {
      return;
    }

    if (method === "Target.targetInfoChanged" && params?.targetInfo) {
      entry.targetInfo = {
        ...entry.targetInfo,
        ...params.targetInfo
      };
    }

    try {
      await sendRelayEvent(method, params, source.sessionId || entry.sessionId);
    } catch (error) {
      if (!relaySocket || relaySocket.readyState !== WebSocket.OPEN) {
        await setActionState(source.tabId, "pending", "Relay reconnecting…");
        return;
      }
      await setActionState(
        source.tabId,
        "error",
        error instanceof Error ? error.message : String(error)
      );
    }
  })();
});

chrome.debugger.onDetach.addListener((source, reason) => {
  void (async () => {
    if (!source.tabId || !attachedTabs.has(source.tabId)) {
      return;
    }
    const tabId = source.tabId;

    if (reason === "canceled_by_user" || reason === "replaced_with_devtools") {
      await detachTab(tabId, reason);
      return;
    }

    const entry = attachedTabs.get(tabId);
    let tabInfo = null;
    try {
      tabInfo = await chrome.tabs.get(tabId);
    } catch {
      await detachTab(tabId, reason);
      return;
    }

    if (
      tabInfo?.url?.startsWith("chrome://") ||
      tabInfo?.url?.startsWith("chrome-extension://")
    ) {
      await detachTab(tabId, reason);
      return;
    }

    if (reattachPending.has(tabId)) {
      return;
    }

    attachedTabs.delete(tabId);
    if (entry) {
      try {
        await sendRelayEvent(
          "Target.detachedFromTarget",
          {
            sessionId: entry.sessionId,
            targetId: entry.targetInfo.targetId,
            reason: "navigation-reattach"
          },
          entry.sessionId
        );
      } catch {
        // relay may be down
      }
    }

    reattachPending.add(tabId);
    await setActionState(tabId, "pending", "Re-attaching after navigation…");
    await persistState();

    const delays = [200, 500, 1000, 2000, 4000];
    for (const delay of delays) {
      await new Promise((resolve) => setTimeout(resolve, delay));

      if (!reattachPending.has(tabId)) {
        return;
      }

      let latestTab = null;
      try {
        latestTab = await chrome.tabs.get(tabId);
      } catch {
        reattachPending.delete(tabId);
        await setActionState(tabId, "", "Attach current tab to IronClaw");
        await persistState();
        return;
      }

      try {
        await attachTab(latestTab, {
          skipAttachedEvent: !(relaySocket && relaySocket.readyState === WebSocket.OPEN)
        });
        reattachPending.delete(tabId);
        if (!relaySocket || relaySocket.readyState !== WebSocket.OPEN) {
          await setActionState(tabId, "pending", "Attached, waiting for relay reconnect…");
        }
        return;
      } catch {
        // continue retries
      }
    }

    reattachPending.delete(tabId);
    await setActionState(tabId, "", "Re-attach failed; click to retry");
    await persistState();
  })();
});

chrome.tabs.onRemoved.addListener((tabId) => {
  void (async () => {
    reattachPending.delete(tabId);
    if (!attachedTabs.has(tabId)) {
      return;
    }
    attachedTabs.delete(tabId);
    await persistState();
  })();
});

chrome.tabs.onUpdated.addListener((tabId, changeInfo, tab) => {
  void (async () => {
    const entry = attachedTabs.get(tabId);
    if (!entry) {
      return;
    }

    const nextTargetInfo = {
      ...entry.targetInfo,
      title:
        typeof changeInfo.title === "string"
          ? changeInfo.title
          : tab.title || entry.targetInfo.title || "",
      url:
        typeof changeInfo.url === "string"
          ? changeInfo.url
          : tab.url || entry.targetInfo.url || ""
    };
    entry.targetInfo = nextTargetInfo;
    await persistState();

    try {
      await sendRelayEvent(
        "Target.targetInfoChanged",
        {
          targetInfo: {
            ...nextTargetInfo,
            attached: true
          }
        },
        entry.sessionId
      );
    } catch {
      // relay may be reconnecting
    }
  })();
});

chrome.webNavigation.onCompleted.addListener(({ tabId, frameId }) => {
  void (async () => {
    if (frameId !== 0 || !attachedTabs.has(tabId)) {
      return;
    }
    await setActionState(
      tabId,
      relaySocket && relaySocket.readyState === WebSocket.OPEN ? "attached" : "pending",
      relaySocket && relaySocket.readyState === WebSocket.OPEN
        ? "Attached to IronClaw"
        : "Relay reconnecting…"
    );
  })();
});

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  void (async () => {
    switch (message?.type) {
      case "relayCheck":
        try {
          const relayUrl = String(message.relayUrl || DEFAULT_RELAY_URL);
          const gatewayToken = String(message.gatewayToken || message.relayToken || "");
          const relayToken =
            String(message.relayToken || "").trim() ||
            (gatewayToken.trim()
              ? await deriveRelayToken(gatewayToken, Number(new URL(relayUrl).port || "80"))
              : "");
          await checkRelayReachable(relayUrl, relayToken);
          sendResponse({ ok: true });
        } catch (error) {
          sendResponse({
            ok: false,
            error: error instanceof Error ? error.message : String(error)
          });
        }
        return;
      case "attachActiveTab":
        try {
          const tab = await resolveMessageTab(message);
          if (!tab?.id) {
            throw new Error("no target tab available");
          }
          if (!attachedTabs.has(tab.id)) {
            cancelReconnect();
            await attachTab(tab);
          }
          const attached = attachedTabs.get(tab.id);
          sendResponse({
            ok: true,
            tabId: tab.id,
            attached: true,
            sessionId: attached?.sessionId || null,
            targetId: attached?.targetInfo?.targetId || null
          });
        } catch (error) {
          sendResponse({
            ok: false,
            error: error instanceof Error ? error.message : String(error)
          });
        }
        return;
      case "detachActiveTab":
        try {
          const tab = await resolveMessageTab(message);
          if (!tab?.id) {
            throw new Error("no target tab available");
          }
          await detachTab(tab.id, "Detached from IronClaw");
          sendResponse({
            ok: true,
            tabId: tab.id,
            attached: false
          });
        } catch (error) {
          sendResponse({
            ok: false,
            error: error instanceof Error ? error.message : String(error)
          });
        }
        return;
      case "extensionState":
        try {
          const tab = await resolveMessageTab(message);
          sendResponse({
            ok: true,
            tabId: tab?.id || null,
            attached: Boolean(tab?.id && attachedTabs.has(tab.id)),
            relayConnected: Boolean(relaySocket && relaySocket.readyState === WebSocket.OPEN),
            relayReconnecting: Boolean(reconnectTimer)
          });
        } catch (error) {
          sendResponse({
            ok: false,
            error: error instanceof Error ? error.message : String(error)
          });
        }
        return;
      default:
        sendResponse({ ok: false, error: "unknown message type" });
        return;
    }
  })();

  return true;
});

chrome.storage.onChanged.addListener((changes, areaName) => {
  if (areaName !== "local") {
    return;
  }
  if (!changes.relayUrl && !changes.relayToken && !changes.gatewayToken) {
    return;
  }
  void (async () => {
    cancelReconnect();
    await closeRelaySocket("relay configuration changed");
    for (const tabId of attachedTabs.keys()) {
      await setActionState(tabId, "error", "Relay config changed; click to reattach");
    }
  })();
});

void rehydrateState();
