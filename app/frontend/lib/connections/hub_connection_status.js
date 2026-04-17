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
