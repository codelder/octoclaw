const RELAY_TOKEN_CONTEXT = "ironclaw-extension-relay-v1";

export function reconnectDelayMs(
  attempt,
  opts = { baseMs: 1000, maxMs: 30000, jitterMs: 1000, random: Math.random }
) {
  const baseMs = Number.isFinite(opts.baseMs) ? opts.baseMs : 1000;
  const maxMs = Number.isFinite(opts.maxMs) ? opts.maxMs : 30000;
  const jitterMs = Number.isFinite(opts.jitterMs) ? opts.jitterMs : 1000;
  const random = typeof opts.random === "function" ? opts.random : Math.random;
  const safeAttempt = Math.max(0, Number.isFinite(attempt) ? attempt : 0);
  const backoff = Math.min(baseMs * 2 ** safeAttempt, maxMs);
  return backoff + Math.max(0, jitterMs) * random();
}

export async function deriveRelayToken(gatewayToken, port) {
  const token = String(gatewayToken || "").trim();
  if (!token) {
    throw new Error("Missing gatewayToken in extension settings");
  }
  const enc = new TextEncoder();
  const key = await crypto.subtle.importKey(
    "raw",
    enc.encode(token),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"]
  );
  const sig = await crypto.subtle.sign(
    "HMAC",
    key,
    enc.encode(`${RELAY_TOKEN_CONTEXT}:${port}`)
  );
  return [...new Uint8Array(sig)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

export async function buildRelayWsUrl(relayUrl, gatewayToken, relayTokenOverride = "") {
  const base = new URL(String(relayUrl || "").trim());
  base.protocol = base.protocol === "https:" ? "wss:" : "ws:";
  base.pathname = "/extension";
  base.search = "";
  const token = String(relayTokenOverride || "").trim() || (await deriveRelayToken(gatewayToken, Number(base.port || "80")));
  base.searchParams.set("token", token);
  return base.toString();
}

export function isRetryableReconnectError(err) {
  const message = err instanceof Error ? err.message : String(err || "");
  if (message.includes("Missing gatewayToken")) {
    return false;
  }
  return true;
}
