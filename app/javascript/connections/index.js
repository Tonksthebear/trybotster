/**
 * Connections module - Global connection management for Turbo-aware lifecycle.
 *
 * Usage:
 *   import { ConnectionManager, HubConnection, TerminalConnection, PreviewConnection } from "connections";
 *
 *   // Hub connection (control plane)
 *   const hub = await ConnectionManager.acquire(HubConnection, hubId, { hubId });
 *   hub.onAgentList((agents) => render(agents));
 *
 *   // Terminal connection (data plane)
 *   const key = TerminalConnection.key(hubId, agentIndex, ptyIndex);
 *   const term = await ConnectionManager.acquire(TerminalConnection, key, {
 *     hubId, agentIndex, ptyIndex
 *   });
 *   term.onOutput((data) => xterm.write(data));
 *
 *   // Preview connection (HTTP proxy)
 *   const previewKey = PreviewConnection.key(hubId, agentIndex, ptyIndex);
 *   const preview = await ConnectionManager.acquire(PreviewConnection, previewKey, {
 *     hubId, agentIndex, ptyIndex
 *   });
 *   const response = await preview.fetch({ method: "GET", path: "/" });
 *
 *   // In controller disconnect():
 *   hub?.release();
 *   term?.release();
 *   preview?.release();
 */

export { ConnectionManager } from "connections/connection_manager";
export { Connection, ConnectionState, BrowserStatus, CliStatus } from "connections/connection";
export { HubConnection } from "connections/hub_connection";
export { TerminalConnection } from "connections/terminal_connection";
export { PreviewConnection } from "connections/preview_connection";
