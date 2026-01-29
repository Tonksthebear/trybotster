/**
 * Connections module - Global connection management for Turbo-aware lifecycle.
 *
 * Usage:
 *   import { ConnectionManager, HubConnection, TerminalConnection } from "connections";
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
 *   // In controller disconnect():
 *   hub?.release();
 *   term?.release();
 */

export { ConnectionManager } from "connections/connection_manager";
export { Connection, ConnectionState } from "connections/connection";
export { HubConnection, HubState } from "connections/hub_connection";
export { TerminalConnection } from "connections/terminal_connection";
