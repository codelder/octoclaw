import { deriveRelayToken } from "./background-utils.js";
import { classifyRelayCheckException, classifyRelayCheckResponse } from "./options-validation.js";

const DEFAULT_RELAY_URL = "http://127.0.0.1:24242";

function setStatus(kind, message) {
  const el = document.getElementById("status");
  if (!el) {
    return;
  }
  el.dataset.kind = kind || "";
  el.textContent = message || "";
}

async function loadConfig() {
  const stored = await chrome.storage.local.get(["relayUrl", "gatewayToken", "relayToken"]);
  document.getElementById("relay-url").value = String(stored.relayUrl || DEFAULT_RELAY_URL);
  document.getElementById("gateway-token").value = String(
    stored.gatewayToken || stored.relayToken || ""
  );
}

async function saveConfig() {
  const relayUrl = document.getElementById("relay-url").value.trim() || DEFAULT_RELAY_URL;
  const gatewayToken = document.getElementById("gateway-token").value.trim();
  await chrome.storage.local.set({ relayUrl, gatewayToken, relayToken: gatewayToken });
  setStatus("ok", `Saved relay settings for ${relayUrl}`);
}

async function testConnection() {
  const relayUrl = document.getElementById("relay-url").value.trim() || DEFAULT_RELAY_URL;
  const gatewayToken = document.getElementById("gateway-token").value.trim();
  if (!gatewayToken) {
    setStatus("error", "Gateway token required. Save your gateway token to connect.");
    return;
  }
  try {
    const port = Number(new URL(relayUrl).port || "80");
    const relayToken = await deriveRelayToken(gatewayToken, port);
    const res = await chrome.runtime.sendMessage({
      type: "relayCheck",
      relayUrl,
      relayToken
    });
    const result = classifyRelayCheckResponse(res, relayUrl);
    if (result.action === "throw") {
      throw new Error(result.error);
    }
    setStatus(result.kind, result.message);
  } catch (error) {
    const result = classifyRelayCheckException(error, relayUrl);
    setStatus(result.kind, result.message);
  }
}

document.getElementById("save").addEventListener("click", () => {
  void saveConfig();
});

document.getElementById("test").addEventListener("click", () => {
  void testConnection();
});

void loadConfig();
