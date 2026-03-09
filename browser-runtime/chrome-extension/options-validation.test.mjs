import test from "node:test";
import assert from "node:assert/strict";
import {
  classifyRelayCheckException,
  classifyRelayCheckResponse
} from "./options-validation.js";

test("maps 401 to gateway token rejected", () => {
  const result = classifyRelayCheckResponse({ status: 401, ok: false }, "http://127.0.0.1:24242");
  assert.deepEqual(result, {
    action: "status",
    kind: "error",
    message: "Gateway token rejected. Check token and save again."
  });
});

test("maps non-json success to wrong endpoint guidance", () => {
  const result = classifyRelayCheckResponse(
    { status: 200, ok: true, contentType: "text/html", json: null },
    "http://127.0.0.1:24242"
  );
  assert.equal(result.kind, "error");
  assert.match(result.message, /Wrong endpoint/);
});

test("maps valid relay json to success", () => {
  const result = classifyRelayCheckResponse(
    {
      status: 200,
      ok: true,
      contentType: "application/json",
      json: { Browser: "IronClaw/extension-relay", "Protocol-Version": "1.3" }
    },
    "http://127.0.0.1:24242"
  );
  assert.deepEqual(result, {
    action: "status",
    kind: "ok",
    message: "Relay reachable and authenticated at http://127.0.0.1:24242"
  });
});

test("maps syntax exceptions to wrong endpoint guidance", () => {
  const result = classifyRelayCheckException(
    new Error("SyntaxError: Unexpected token <"),
    "http://127.0.0.1:24242"
  );
  assert.equal(result.kind, "error");
  assert.match(result.message, /Wrong endpoint/);
});
