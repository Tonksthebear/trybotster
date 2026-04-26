/**
 * Connections module - Global connection management for client-side route changes.
 *
 * Usage:
 *   import { HubConnectionManager, HubManager, HubTransport, TerminalConnection } from "connections";
 *
 *   // Hub control plane is owned by store/hub-store.js via lib/hub-bridge.js.
 *   // React components should await that shared session with waitForHub(hubId),
 *   // not acquire their own HubTransport or read HubManager directly.
 *
 *   // Terminal connection (data plane)
 *   const key = TerminalConnection.key(hubId, sessionUuid);
 *   const term = await HubConnectionManager.acquire(TerminalConnection, key, {
 *     hubId, sessionUuid
 *   });
 *   term.onOutput((data) => terminal.write(data));
 *
 *   // On component unmount:
 *   hub?.release();
 *   term?.release();
 */

export { HubConnectionManager } from "connections/hub_connection_manager";
export { HubManager } from "connections/hub_manager";
export { Hub, HubSession } from "connections/hub";
export { HubRoute, ConnectionState, BrowserStatus, CliStatus, ConnectionMode } from "connections/hub_route";
export { HubTransport } from "connections/hub_connection";
export { TerminalConnection } from "connections/terminal_connection";
