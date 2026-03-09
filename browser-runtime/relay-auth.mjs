import { createHmac } from "node:crypto";

const RELAY_TOKEN_CONTEXT = "ironclaw-extension-relay-v1";
const DEFAULT_RELAY_PROBE_TIMEOUT_MS = 500;
export const RELAY_AUTH_HEADER = "x-ironclaw-relay-token";
export const RELAY_BROWSER_IDENTITY = "IronClaw/extension-relay";

function trimToUndefined(value) {
  if (typeof value !== "string") {
    return undefined;
  }
  const trimmed = value.trim();
  return trimmed ? trimmed : undefined;
}

export function deriveRelayToken(gatewayToken, port) {
  const trimmed = trimToUndefined(gatewayToken);
  if (!trimmed) {
    throw new Error("Missing gatewayToken");
  }
  return createHmac("sha256", trimmed)
    .update(`${RELAY_TOKEN_CONTEXT}:${port}`)
    .digest("hex");
}

export function resolveRelayAcceptedTokens({ port, relayToken, gatewayToken }) {
  const accepted = new Set();
  const explicitRelayToken = trimToUndefined(relayToken);
  if (explicitRelayToken) {
    accepted.add(explicitRelayToken);
  }

  const trimmedGatewayToken = trimToUndefined(gatewayToken);
  if (trimmedGatewayToken) {
    accepted.add(deriveRelayToken(trimmedGatewayToken, port));
    accepted.add(trimmedGatewayToken);
  }

  return accepted;
}

export function getRelayAuthTokenFromRequest(req, url, acceptedTokens) {
  const headerValue = req.headers[RELAY_AUTH_HEADER];
  const header = Array.isArray(headerValue) ? headerValue[0] : headerValue;
  const token = (typeof header === "string" && header.trim()) || url.searchParams.get("token")?.trim();
  if (!token) {
    return null;
  }
  return acceptedTokens.has(token) ? token : null;
}

export async function probeAuthenticatedIronClawRelay({
  baseUrl,
  relayAuthHeader = RELAY_AUTH_HEADER,
  relayAuthToken,
  timeoutMs = DEFAULT_RELAY_PROBE_TIMEOUT_MS
}) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const versionUrl = new URL("/json/version", `${baseUrl}/`).toString();
    const res = await fetch(versionUrl, {
      signal: controller.signal,
      headers: relayAuthToken ? { [relayAuthHeader]: relayAuthToken } : {}
    });
    if (!res.ok) {
      return false;
    }
    const body = await res.json();
    const browserName = typeof body?.Browser === "string" ? body.Browser.trim() : "";
    return browserName === RELAY_BROWSER_IDENTITY;
  } catch {
    return false;
  } finally {
    clearTimeout(timer);
  }
}
