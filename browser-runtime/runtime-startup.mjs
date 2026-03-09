import { deriveRelayToken, probeAuthenticatedIronClawRelay } from "./relay-auth.mjs";

function trimToUndefined(value) {
  if (typeof value !== "string") {
    return undefined;
  }
  const trimmed = value.trim();
  return trimmed ? trimmed : undefined;
}

export function resolveRelayProbeToken({ port, relayToken, gatewayToken }) {
  const explicitRelayToken = trimToUndefined(relayToken);
  if (explicitRelayToken) {
    return explicitRelayToken;
  }

  const trimmedGatewayToken = trimToUndefined(gatewayToken);
  if (!trimmedGatewayToken) {
    return undefined;
  }

  return deriveRelayToken(trimmedGatewayToken, port);
}

export async function probeCompatibleRuntime({ host, port, relayToken, gatewayToken }) {
  const baseUrl = `http://${host}:${port}`;
  const relayAuthToken = resolveRelayProbeToken({ port, relayToken, gatewayToken });
  const compatible = await probeAuthenticatedIronClawRelay({
    baseUrl,
    relayAuthToken
  });
  return {
    baseUrl,
    relayAuthToken,
    compatible
  };
}

export async function listenWithCompatibleRuntimeProbe({
  server,
  host,
  port,
  relayToken,
  gatewayToken,
  onReuse
}) {
  return await new Promise((resolve, reject) => {
    let settled = false;

    const cleanup = () => {
      server.off("error", onError);
      server.off("listening", onListening);
    };

    const onListening = () => {
      if (settled) {
        return;
      }
      settled = true;
      cleanup();
      resolve({
        reused: false,
        baseUrl: `http://${host}:${port}`,
        mode: "listening"
      });
    };

    const onError = (error) => {
      if (settled) {
        return;
      }
      if (error?.code !== "EADDRINUSE") {
        settled = true;
        cleanup();
        reject(error);
        return;
      }

      (async () => {
        try {
          const probe = await probeCompatibleRuntime({
            host,
            port,
            relayToken,
            gatewayToken
          });
          if (!probe.compatible) {
            settled = true;
            cleanup();
            reject(error);
            return;
          }
          if (typeof onReuse === "function") {
            await onReuse(probe);
          }
          settled = true;
          cleanup();
          resolve({
            reused: true,
            mode: "reused-existing",
            ...probe
          });
        } catch (probeError) {
          settled = true;
          cleanup();
          reject(probeError);
        }
      })();
    };

    server.once("error", onError);
    server.once("listening", onListening);
    server.listen(port, host);
  });
}
