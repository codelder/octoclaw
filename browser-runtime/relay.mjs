import { WebSocketServer } from "ws";
import {
  RELAY_AUTH_HEADER,
  RELAY_BROWSER_IDENTITY,
  getRelayAuthTokenFromRequest,
  resolveRelayAcceptedTokens
} from "./relay-auth.mjs";

const DEFAULT_EXTENSION_RECONNECT_GRACE_MS = 20_000;
const DEFAULT_EXTENSION_COMMAND_RECONNECT_WAIT_MS = 3_000;

function rejectUpgrade(socket, status, bodyText) {
  const body = Buffer.from(bodyText);
  socket.write(
    `HTTP/1.1 ${status} ${status === 200 ? "OK" : "ERR"}\r\n` +
      "Content-Type: text/plain; charset=utf-8\r\n" +
      `Content-Length: ${body.length}\r\n` +
      "Connection: close\r\n\r\n"
  );
  socket.write(body);
  socket.end();
}

function envMsOrDefault(name, fallback) {
  const raw = process.env[name];
  if (!raw || !raw.trim()) {
    return fallback;
  }
  const parsed = Number.parseInt(raw, 10);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    return fallback;
  }
  return parsed;
}

function headerValue(value) {
  if (!value) {
    return undefined;
  }
  if (Array.isArray(value)) {
    return value[0];
  }
  return value;
}

export function installExtensionRelay(_server, { baseUrl, host, port, relayToken, gatewayToken }) {
  let extensionWs = null;
  const cdpClients = new Set();
  const connectedTargets = new Map();
  const pendingExtension = new Map();
  let nextExtensionId = 1;
  const acceptedTokens = resolveRelayAcceptedTokens({ port, relayToken, gatewayToken });
  const extensionReconnectGraceMs = envMsOrDefault(
    "IRONCLAW_EXTENSION_RELAY_RECONNECT_GRACE_MS",
    DEFAULT_EXTENSION_RECONNECT_GRACE_MS
  );
  const extensionCommandReconnectWaitMs = envMsOrDefault(
    "IRONCLAW_EXTENSION_RELAY_COMMAND_RECONNECT_WAIT_MS",
    DEFAULT_EXTENSION_COMMAND_RECONNECT_WAIT_MS
  );
  let extensionDisconnectCleanupTimer = null;
  const extensionReconnectWaiters = new Set();

  const wssExtension = new WebSocketServer({ noServer: true });
  const wssCdp = new WebSocketServer({ noServer: true });

  const extensionConnected = () => extensionWs?.readyState === 1;
  const hasConnectedTargets = () => connectedTargets.size > 0;
  const cdpWsUrl = `ws://${host}:${port}/cdp`;

  const flushExtensionReconnectWaiters = (connected) => {
    if (extensionReconnectWaiters.size === 0) {
      return;
    }
    const waiters = Array.from(extensionReconnectWaiters);
    extensionReconnectWaiters.clear();
    for (const waiter of waiters) {
      waiter(connected);
    }
  };

  const clearExtensionDisconnectCleanupTimer = () => {
    if (!extensionDisconnectCleanupTimer) {
      return;
    }
    clearTimeout(extensionDisconnectCleanupTimer);
    extensionDisconnectCleanupTimer = null;
  };

  const closeCdpClientsAfterExtensionDisconnect = () => {
    connectedTargets.clear();
    for (const client of cdpClients) {
      try {
        client.close(1011, "extension disconnected");
      } catch {}
    }
    cdpClients.clear();
    flushExtensionReconnectWaiters(false);
  };

  const scheduleExtensionDisconnectCleanup = () => {
    clearExtensionDisconnectCleanupTimer();
    extensionDisconnectCleanupTimer = setTimeout(() => {
      extensionDisconnectCleanupTimer = null;
      if (extensionConnected()) {
        return;
      }
      closeCdpClientsAfterExtensionDisconnect();
    }, extensionReconnectGraceMs);
  };

  const waitForExtensionReconnect = async (timeoutMs) => {
    if (extensionConnected()) {
      return true;
    }
    return await new Promise((resolve) => {
      let settled = false;
      const waiter = (connected) => {
        if (settled) {
          return;
        }
        settled = true;
        clearTimeout(timer);
        extensionReconnectWaiters.delete(waiter);
        resolve(connected);
      };
      const timer = setTimeout(() => waiter(false), timeoutMs);
      extensionReconnectWaiters.add(waiter);
    });
  };

  const broadcastToCdpClients = (evt) => {
    const payload = JSON.stringify(evt);
    for (const ws of cdpClients) {
      if (ws.readyState === 1) {
        ws.send(payload);
      }
    }
  };

  const dropConnectedTargetSession = (sessionId) => {
    const existing = connectedTargets.get(sessionId);
    if (!existing) {
      return undefined;
    }
    connectedTargets.delete(sessionId);
    return existing;
  };

  const dropConnectedTargetsByTargetId = (targetId) => {
    const removed = [];
    for (const [sessionId, target] of connectedTargets) {
      if (target.targetId !== targetId) {
        continue;
      }
      connectedTargets.delete(sessionId);
      removed.push(target);
    }
    return removed;
  };

  const broadcastDetachedTarget = (target, targetId) => {
    broadcastToCdpClients({
      method: "Target.detachedFromTarget",
      params: {
        sessionId: target.sessionId,
        targetId: targetId || target.targetId
      },
      sessionId: target.sessionId
    });
  };

  const isMissingTargetError = (err) => {
    const message = (err instanceof Error ? err.message : String(err || "")).toLowerCase();
    return (
      message.includes("target not found") ||
      message.includes("no target with given id") ||
      message.includes("session not found") ||
      message.includes("cannot find session")
    );
  };

  const pruneStaleTargetsFromCommandFailure = (cmd, err) => {
    if (!isMissingTargetError(err)) {
      return;
    }
    if (cmd.sessionId) {
      const removed = dropConnectedTargetSession(cmd.sessionId);
      if (removed) {
        broadcastDetachedTarget(removed);
        return;
      }
    }
    const targetId = typeof cmd.params?.targetId === "string" ? cmd.params.targetId : undefined;
    if (!targetId) {
      return;
    }
    const removedTargets = dropConnectedTargetsByTargetId(targetId);
    for (const removed of removedTargets) {
      broadcastDetachedTarget(removed, targetId);
    }
  };

  const ensureTargetEventsForClient = (ws) => {
    for (const target of connectedTargets.values()) {
      ws.send(
        JSON.stringify({
          method: "Target.attachedToTarget",
          params: {
            sessionId: target.sessionId,
            targetInfo: { ...target.targetInfo, attached: true },
            waitingForDebugger: false
          }
        })
      );
    }
  };

  const sendToExtension = async (payload) => {
    let ws = extensionWs;
    if (!ws || ws.readyState !== 1) {
      const reconnected = await waitForExtensionReconnect(extensionCommandReconnectWaitMs);
      if (!reconnected) {
        throw new Error("Chrome relay extension not connected");
      }
      ws = extensionWs;
    }
    if (!ws || ws.readyState !== 1) {
      throw new Error("Chrome relay extension not connected");
    }

    ws.send(JSON.stringify(payload));
    return await new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        pendingExtension.delete(payload.id);
        reject(new Error(`extension request timeout: ${payload.params.method}`));
      }, 30000);
      pendingExtension.set(payload.id, { resolve, reject, timer });
    });
  };

  const sendBestEffortToExtension = (payload) => {
    const ws = extensionWs;
    if (!ws || ws.readyState !== 1) {
      return false;
    }
    ws.send(JSON.stringify(payload));
    return true;
  };

  const routeCdpCommand = async (cmd) => {
    switch (cmd.method) {
      case "Browser.getVersion":
        return {
          protocolVersion: "1.3",
          product: "Chrome/IronClaw-Extension-Relay",
          revision: "0",
          userAgent: "IronClaw-Extension-Relay",
          jsVersion: "V8"
        };
      case "Browser.setDownloadBehavior":
      case "Target.setAutoAttach":
      case "Target.setDiscoverTargets":
        return {};
      case "Target.getTargets":
        return {
          targetInfos: Array.from(connectedTargets.values()).map((t) => ({
            ...t.targetInfo,
            attached: true
          }))
        };
      case "Target.getTargetInfo": {
        const targetId = cmd.params?.targetId;
        const target = Array.from(connectedTargets.values()).find((t) => t.targetId === targetId);
        if (target) {
          return { targetInfo: target.targetInfo };
        }
        if (cmd.sessionId && connectedTargets.has(cmd.sessionId)) {
          return { targetInfo: connectedTargets.get(cmd.sessionId)?.targetInfo };
        }
        return { targetInfo: Array.from(connectedTargets.values())[0]?.targetInfo };
      }
      case "Target.attachToTarget": {
        const targetId = cmd.params?.targetId;
        const target = Array.from(connectedTargets.values()).find((t) => t.targetId === targetId);
        if (!target) {
          throw new Error("target not found");
        }
        return { sessionId: target.sessionId };
      }
      default:
        return await sendToExtension({
          id: nextExtensionId++,
          method: "forwardCDPCommand",
          params: {
            method: cmd.method,
            params: cmd.params,
            sessionId: cmd.sessionId
          }
        });
    }
  };

  const handleHttp = (req, res) => {
    const url = new URL(req.url ?? "/", baseUrl);
    const path = url.pathname;
    const origin = headerValue(req.headers.origin);
    const isChromeExtensionOrigin =
      typeof origin === "string" && origin.startsWith("chrome-extension://");

    if (isChromeExtensionOrigin && origin) {
      res.setHeader("Access-Control-Allow-Origin", origin);
      res.setHeader("Vary", "Origin");
    }

    if (req.method === "OPTIONS") {
      if (origin && !isChromeExtensionOrigin) {
        res.writeHead(403);
        res.end("Forbidden");
        return true;
      }
      const requestedHeaders = headerValue(req.headers["access-control-request-headers"])
        ?.split(",")
        .map((header) => header.trim().toLowerCase())
        .filter((header) => header.length > 0) ?? [];
      const allowedHeaders = new Set(["content-type", RELAY_AUTH_HEADER, ...requestedHeaders]);
      res.writeHead(204, {
        "Access-Control-Allow-Origin": origin ?? "*",
        "Access-Control-Allow-Methods": "GET, PUT, POST, OPTIONS",
        "Access-Control-Allow-Headers": Array.from(allowedHeaders).join(", "),
        "Access-Control-Max-Age": "86400",
        Vary: "Origin, Access-Control-Request-Headers"
      });
      res.end();
      return true;
    }

    if (req.method === "HEAD" && path === "/") {
      res.writeHead(200);
      res.end();
      return true;
    }

    if (path === "/") {
      res.writeHead(200, { "Content-Type": "text/plain; charset=utf-8" });
      res.end("OK");
      return true;
    }

    if (path === "/extension/status") {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ connected: extensionConnected(), targetCount: connectedTargets.size }));
      return true;
    }

    const relayAuthed = getRelayAuthTokenFromRequest(req, url, acceptedTokens);
    if (path.startsWith("/json") && !relayAuthed) {
      res.writeHead(401);
      res.end("Unauthorized");
      return true;
    }

    if (
      (path === "/json/version" || path === "/json/version/") &&
      (req.method === "GET" || req.method === "PUT")
    ) {
      const payload = {
        Browser: RELAY_BROWSER_IDENTITY,
        "Protocol-Version": "1.3"
      };
      if (extensionConnected() || hasConnectedTargets()) {
        payload.webSocketDebuggerUrl = cdpWsUrl;
      }
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify(payload));
      return true;
    }

    if (
      (path === "/json" || path === "/json/" || path === "/json/list" || path === "/json/list/") &&
      (req.method === "GET" || req.method === "PUT")
    ) {
      const list = Array.from(connectedTargets.values()).map((t) => ({
        id: t.targetId,
        type: t.targetInfo.type ?? "page",
        title: t.targetInfo.title ?? "",
        description: t.targetInfo.title ?? "",
        url: t.targetInfo.url ?? "",
        webSocketDebuggerUrl: cdpWsUrl,
        devtoolsFrontendUrl: `/devtools/inspector.html?ws=${cdpWsUrl.replace("ws://", "")}`
      }));
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify(list));
      return true;
    }

    const handleTargetActionRoute = (match, cdpMethod) => {
      if (!match || (req.method !== "GET" && req.method !== "PUT")) {
        return false;
      }
      let targetId = "";
      try {
        targetId = decodeURIComponent(match[1] ?? "").trim();
      } catch {
        res.writeHead(400);
        res.end("invalid targetId encoding");
        return true;
      }
      if (!targetId) {
        res.writeHead(400);
        res.end("targetId required");
        return true;
      }
      sendBestEffortToExtension({
        id: nextExtensionId++,
        method: "forwardCDPCommand",
        params: { method: cdpMethod, params: { targetId } }
      });
      res.writeHead(200);
      res.end("OK");
      return true;
    };

    if (handleTargetActionRoute(path.match(/^\/json\/activate\/(.+)$/), "Target.activateTarget")) {
      return true;
    }
    if (handleTargetActionRoute(path.match(/^\/json\/close\/(.+)$/), "Target.closeTarget")) {
      return true;
    }

    return false;
  };

  const handleUpgrade = (req, socket, head) => {
    const url = new URL(req.url ?? "/", baseUrl);
    const path = url.pathname;
    const authed = getRelayAuthTokenFromRequest(req, url, acceptedTokens);
    if (path === "/extension") {
      if (!authed) {
        rejectUpgrade(socket, 401, "Unauthorized");
        return true;
      }
      if (extensionWs && extensionWs.readyState !== 1) {
        try {
          extensionWs.terminate();
        } catch {}
        extensionWs = null;
      }
      if (extensionConnected()) {
        rejectUpgrade(socket, 409, "Extension already connected");
        return true;
      }
      wssExtension.handleUpgrade(req, socket, head, (ws) => {
        wssExtension.emit("connection", ws, req);
      });
      return true;
    }
    if (path === "/cdp") {
      if (!authed) {
        rejectUpgrade(socket, 401, "Unauthorized");
        return true;
      }
      wssCdp.handleUpgrade(req, socket, head, (ws) => {
        wssCdp.emit("connection", ws, req);
      });
      return true;
    }
    return false;
  };

  wssExtension.on("connection", (ws) => {
    extensionWs = ws;
    clearExtensionDisconnectCleanupTimer();
    flushExtensionReconnectWaiters(true);
    const ping = setInterval(() => {
      if (ws.readyState !== 1) {
        return;
      }
      ws.send(JSON.stringify({ method: "ping" }));
    }, 5000);

    ws.on("message", (data) => {
      let parsed = null;
      try {
        parsed = JSON.parse(String(data));
      } catch {
        return;
      }

      if (parsed && typeof parsed.id === "number") {
        const pending = pendingExtension.get(parsed.id);
        if (!pending) {
          return;
        }
        pendingExtension.delete(parsed.id);
        clearTimeout(pending.timer);
        if (parsed.error) {
          pending.reject(new Error(String(parsed.error)));
        } else {
          pending.resolve(parsed.result);
        }
        return;
      }

      if (parsed?.method === "pong") {
        return;
      }

      if (parsed?.method !== "forwardCDPEvent") {
        return;
      }

      const method = parsed.params?.method;
      const params = parsed.params?.params;
      const sessionId = parsed.params?.sessionId;

      if (method === "Target.attachedToTarget" && params?.sessionId && params?.targetInfo?.targetId) {
        const prev = connectedTargets.get(params.sessionId);
        const nextTargetId = params.targetInfo.targetId;
        const prevTargetId = prev?.targetId;
        const changedTarget = Boolean(prev && prevTargetId && prevTargetId !== nextTargetId);
        connectedTargets.set(params.sessionId, {
          sessionId: params.sessionId,
          targetId: nextTargetId,
          targetInfo: params.targetInfo
        });
        if (changedTarget && prevTargetId) {
          broadcastDetachedTarget({ sessionId: params.sessionId, targetId: prevTargetId }, prevTargetId);
        }
        if (!prev || changedTarget) {
          broadcastToCdpClients({ method, params, sessionId });
        }
        return;
      }

      if (method === "Target.detachedFromTarget") {
        if (params?.sessionId) {
          dropConnectedTargetSession(params.sessionId);
        } else if (params?.targetId) {
          dropConnectedTargetsByTargetId(params.targetId);
        }
        broadcastToCdpClients({ method, params, sessionId });
        return;
      }

      if ((method === "Target.targetDestroyed" || method === "Target.targetCrashed") && params?.targetId) {
        dropConnectedTargetsByTargetId(params.targetId);
        broadcastToCdpClients({ method, params, sessionId });
        return;
      }

      if (method === "Target.targetInfoChanged" && params?.targetInfo?.targetId) {
        for (const [sid, target] of connectedTargets) {
          if (target.targetId !== params.targetInfo.targetId) {
            continue;
          }
          connectedTargets.set(sid, {
            ...target,
            targetInfo: { ...target.targetInfo, ...params.targetInfo }
          });
        }
      }

      broadcastToCdpClients({ method, params, sessionId });
    });

    ws.on("close", () => {
      clearInterval(ping);
      if (extensionWs === ws) {
        extensionWs = null;
      }
      for (const [, pending] of pendingExtension) {
        clearTimeout(pending.timer);
        pending.reject(new Error("extension disconnected"));
      }
      pendingExtension.clear();
      scheduleExtensionDisconnectCleanup();
    });
  });

  wssCdp.on("connection", (ws) => {
    cdpClients.add(ws);
    ensureTargetEventsForClient(ws);

    ws.on("message", async (data) => {
      let cmd = null;
      try {
        cmd = JSON.parse(String(data));
      } catch {
        return;
      }
      if (!cmd || typeof cmd.id !== "number" || typeof cmd.method !== "string") {
        return;
      }

      try {
        const result = await routeCdpCommand(cmd);
        if (cmd.method === "Target.setAutoAttach" || cmd.method === "Target.setDiscoverTargets") {
          ensureTargetEventsForClient(ws);
        }
        ws.send(JSON.stringify({ id: cmd.id, sessionId: cmd.sessionId, result }));
      } catch (err) {
        pruneStaleTargetsFromCommandFailure(cmd, err);
        ws.send(
          JSON.stringify({
            id: cmd.id,
            sessionId: cmd.sessionId,
            error: { message: err instanceof Error ? err.message : String(err) }
          })
        );
      }
    });

    ws.on("close", () => {
      cdpClients.delete(ws);
    });
  });

  const relayInfo = {
    relayToken: relayToken || null,
    relayAuthHeader: RELAY_AUTH_HEADER,
    extensionConnected,
    relayBaseUrl: baseUrl,
    relayWsUrl: cdpWsUrl,
    acceptedTokenCount: acceptedTokens.size,
    extensionReconnectGraceMs,
    extensionCommandReconnectWaitMs
  };

  return {
    handleHttp,
    handleUpgrade,
    relayInfo
  };
}
