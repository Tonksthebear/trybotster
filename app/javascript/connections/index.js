/**
 * Connections module - Global connection management for Turbo-aware lifecycle.
 *
 * Usage:
 *   import { HubConnectionManager, HubConnection, TerminalConnection, PreviewConnection } from "connections";
 *
 *   // Hub connection (control plane)
 *   const hub = await HubConnectionManager.acquire(HubConnection, hubId, { hubId });
 *   hub.onAgentList((agents) => render(agents));
 *
 *   // Terminal connection (data plane)
 *   const key = TerminalConnection.key(hubId, sessionUuid);
 *   const term = await HubConnectionManager.acquire(TerminalConnection, key, {
 *     hubId, sessionUuid
 *   });
 *   term.onOutput((data) => terminal.write(data));
 *
 *   // Preview connection (HTTP proxy)
 *   const previewKey = PreviewConnection.key(hubId, sessionUuid);
 *   const preview = await HubConnectionManager.acquire(PreviewConnection, previewKey, {
 *     hubId, sessionUuid
 *   });
 *   const response = await preview.fetch({ method: "GET", path: "/" });
 *
 *   // In controller disconnect():
 *   hub?.release();
 *   term?.release();
 *   preview?.release();
 */

export { HubConnectionManager } from "connections/hub_connection_manager";
export { HubRoute, ConnectionState, BrowserStatus, CliStatus, ConnectionMode } from "connections/hub_route";
export { HubConnection } from "connections/hub_connection";
export { TerminalConnection } from "connections/terminal_connection";
export { PreviewConnection } from "connections/preview_connection";
