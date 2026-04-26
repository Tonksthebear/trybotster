import { CliStatus, ConnectionMode } from "connections/constants";

export const DEFAULT_HUB_CONNECTION_STATUS = Object.freeze({
  browser: "connecting",
  connection: "disconnected",
  hub: null,
  details: {
    browserSocketState: "connecting",
    cliStatus: CliStatus.UNKNOWN,
    routeState: "disconnected",
    connectionMode: ConnectionMode.UNKNOWN,
    errorCode: null,
    shouldAttemptWebRtc: false,
  },
});

function browserBadgeStatus(socketState) {
  if (socketState === "connected") return "connected";
  if (socketState === "connecting") return "connecting";
  if (socketState === "error") return "error";
  return "disconnected";
}

function hubBadgeStatus(cliStatus) {
  if (cliStatus === CliStatus.UNKNOWN) return null;
  if (cliStatus === CliStatus.OFFLINE || cliStatus === CliStatus.DISCONNECTED) {
    return "offline";
  }
  return "online";
}

function hubIsOnline(cliStatus) {
  return hubBadgeStatus(cliStatus) === "online";
}

/**
 * Combine the transport-derived hub status with the local hub entity's
 * recovery_state to produce the dot the sidebar renders.
 *
 * Priority (highest to lowest):
 *   1. transport says "offline"  → "offline"   (paired-then-dropped beats local)
 *   2. transport says "online"   → "online"
 *   3. local hub entity is ready → "online"    (unpaired-but-running shows green)
 *   4. otherwise                 → null        (renders as "connecting")
 *
 * Health events from Rails ActionCable only fire after the hub is paired,
 * so an unpaired-but-locally-running hub would otherwise stay amber forever.
 * The local `hub` entity (cli/lua/hub/init.lua) ships
 * `recovery_state.state === "ready"` once the hub finishes startup, which we
 * treat as proof of liveness independent of the server-side health channel.
 *
 * @param {string|null|undefined} transportHubStatus - from hubBadgeStatus().
 * @param {boolean} entityReady - hubEntity?.state === "ready".
 * @returns {"online"|"offline"|null}
 */
export function resolveHubStatus(transportHubStatus, entityReady) {
  if (transportHubStatus === "offline") return "offline";
  if (transportHubStatus === "online") return "online";
  if (entityReady) return "online";
  return null;
}

function browserSocketConnected(socketState) {
  return socketState === "connected";
}

function connectionBadgeState({ errorCode, connected, shouldAttemptWebRtc, connectionMode }) {
  if (errorCode === "unpaired") return "unpaired";
  if (errorCode === "session_invalid") return "expired";
  if (connected) {
    if (connectionMode === ConnectionMode.DIRECT) return "direct";
    if (connectionMode === ConnectionMode.RELAYED) return "relay";
    return "connecting";
  }
  return shouldAttemptWebRtc ? "connecting" : "disconnected";
}

export function buildHubConnectionStatus(transport) {
  const browserSocketState = transport?.browserSocketState || "disconnected";
  const cliStatus = transport?.cliStatus || CliStatus.UNKNOWN;
  const routeState = transport?.state || "disconnected";
  const connectionMode = transport?.connectionMode || ConnectionMode.UNKNOWN;
  const errorCode = transport?.errorCode || null;
  const connected = transport?.isConnected?.() ?? false;
  const shouldAttemptWebRtc =
    !errorCode &&
    browserSocketConnected(browserSocketState) &&
    hubIsOnline(cliStatus);

  return {
    browser: browserBadgeStatus(browserSocketState),
    connection: connectionBadgeState({
      errorCode,
      connected,
      shouldAttemptWebRtc,
      connectionMode,
    }),
    hub: hubBadgeStatus(cliStatus),
    details: {
      browserSocketState,
      cliStatus,
      routeState,
      connectionMode,
      errorCode,
      shouldAttemptWebRtc,
    },
  };
}
