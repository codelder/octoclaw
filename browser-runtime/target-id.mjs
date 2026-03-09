export function resolveTargetIdFromCandidates(rawTargetId, candidates) {
  const targetId = String(rawTargetId || "").trim();
  if (!targetId) {
    return { ok: false, reason: "missing" };
  }

  if (candidates.includes(targetId)) {
    return { ok: true, targetId };
  }

  const matches = candidates.filter((candidate) => candidate.startsWith(targetId));
  if (matches.length === 1) {
    return { ok: true, targetId: matches[0] };
  }
  if (matches.length > 1) {
    return { ok: false, reason: "ambiguous" };
  }
  return { ok: false, reason: "missing" };
}
