import test from "node:test";
import assert from "node:assert/strict";
import { resolveTargetIdFromCandidates } from "./target-id.mjs";

test("target id resolution supports exact ids, unique prefixes, and ambiguous prefixes", () => {
  assert.deepEqual(resolveTargetIdFromCandidates("", ["abc"]), {
    ok: false,
    reason: "missing"
  });

  assert.deepEqual(resolveTargetIdFromCandidates("tab-123", ["tab-123", "tab-456"]), {
    ok: true,
    targetId: "tab-123"
  });

  assert.deepEqual(resolveTargetIdFromCandidates("tab-12", ["tab-123", "tab-456"]), {
    ok: true,
    targetId: "tab-123"
  });

  assert.deepEqual(resolveTargetIdFromCandidates("tab-", ["tab-123", "tab-456"]), {
    ok: false,
    reason: "ambiguous"
  });

  assert.deepEqual(resolveTargetIdFromCandidates("missing", ["tab-123", "tab-456"]), {
    ok: false,
    reason: "missing"
  });
});
