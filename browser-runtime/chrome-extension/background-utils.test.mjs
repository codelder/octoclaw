import test from "node:test";
import assert from "node:assert/strict";
import {
  buildRelayWsUrl,
  deriveRelayToken,
  isRetryableReconnectError,
  reconnectDelayMs
} from "./background-utils.js";

test("deriveRelayToken returns deterministic hmac per port", async () => {
  const token = await deriveRelayToken("test-gateway-token", 24242);
  const same = await deriveRelayToken("test-gateway-token", 24242);
  const other = await deriveRelayToken("test-gateway-token", 24243);
  assert.equal(token, same);
  assert.notEqual(token, other);
  assert.match(token, /^[0-9a-f]{64}$/);
});

test("buildRelayWsUrl uses derived relay token", async () => {
  const url = await buildRelayWsUrl("http://127.0.0.1:24242", "test-gateway-token");
  assert.match(url, /^ws:\/\/127\.0\.0\.1:24242\/extension\?token=[0-9a-f]{64}$/);
});

test("reconnectDelayMs applies exponential backoff", () => {
  assert.equal(reconnectDelayMs(0, { baseMs: 1000, maxMs: 30000, jitterMs: 0, random: () => 0 }), 1000);
  assert.equal(reconnectDelayMs(1, { baseMs: 1000, maxMs: 30000, jitterMs: 0, random: () => 0 }), 2000);
});

test("missing gateway token is non-retryable", () => {
  assert.equal(
    isRetryableReconnectError(new Error("Missing gatewayToken in extension settings")),
    false
  );
});
