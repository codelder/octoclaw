const PORT_GUIDANCE = "Use the relay port, not the main gateway port.";

function hasCdpVersionShape(data) {
  return !!data && typeof data === "object" && "Browser" in data && "Protocol-Version" in data;
}

export function classifyRelayCheckResponse(res, relayUrl) {
  if (!res) {
    return { action: "throw", error: "No response from service worker" };
  }

  if (res.status === 401) {
    return {
      action: "status",
      kind: "error",
      message: "Gateway token rejected. Check token and save again."
    };
  }

  if (res.error) {
    return { action: "throw", error: res.error };
  }

  if (!res.ok) {
    return { action: "throw", error: `HTTP ${res.status}` };
  }

  const contentType = String(res.contentType || "");
  if (!contentType.includes("application/json")) {
    return {
      action: "status",
      kind: "error",
      message: `Wrong endpoint: this looks like HTML/text, not relay JSON. ${PORT_GUIDANCE}`
    };
  }

  if (!hasCdpVersionShape(res.json)) {
    return {
      action: "status",
      kind: "error",
      message: `Wrong endpoint: expected relay /json/version response. ${PORT_GUIDANCE}`
    };
  }

  return {
    action: "status",
    kind: "ok",
    message: `Relay reachable and authenticated at ${relayUrl}`
  };
}

export function classifyRelayCheckException(err, relayUrl) {
  const message = String(err || "").toLowerCase();
  if (message.includes("json") || message.includes("syntax")) {
    return {
      kind: "error",
      message: `Wrong endpoint: this is not a relay JSON endpoint. ${PORT_GUIDANCE}`
    };
  }

  return {
    kind: "error",
    message: `Relay not reachable/authenticated at ${relayUrl}. Start IronClaw browser relay and verify token.`
  };
}
