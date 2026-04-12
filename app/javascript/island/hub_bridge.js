import { HubManager } from "connections";
import { normalize, selectedSessionId, setSurface } from "island/store";

let hub = null;
let unsubscribers = [];

// Connect the signal store to a hub's live state.
// Call once from the Stimulus mount controller.
export async function connect(hubId, { surface = "panel" } = {}) {
  if (hub) disconnect();

  setSurface(surface);
  hub = await HubManager.acquire(hubId);

  // Hydrate from current state (always normalize — clears stale data from prior hub)
  normalize(hub.agents.current(), hub.openWorkspaces.current());

  // Trigger loads (may already be cached from other controllers sharing the hub)
  hub.agents.load().catch(() => {});
  hub.openWorkspaces.load().catch(() => {});

  // Subscribe to live updates — re-sync selection after each normalize
  // because agents may arrive after initial connect when selection was null
  unsubscribers.push(
    hub.agents.onChange((agents) => {
      const workspaces = hub.openWorkspaces.current();
      normalize(agents, workspaces);
      syncSelectionFromUrl();
    }),
  );

  unsubscribers.push(
    hub.openWorkspaces.onChange((workspaces) => {
      const agents = hub.agents.current();
      normalize(agents, workspaces);
    }),
  );

  // Sync selection from URL
  syncSelectionFromUrl();

  return hub;
}

export function disconnect() {
  unsubscribers.forEach((unsub) => unsub());
  unsubscribers = [];

  const ref = hub;
  hub = null;
  ref?.release();
}

export function getHub() {
  return hub;
}

// Derive selected session from current URL path
export function syncSelectionFromUrl() {
  const match = window.location.pathname.match(
    /\/hubs\/[^/]+\/sessions\/([^/]+)/,
  );
  if (!match) {
    selectedSessionId.value = null;
    return;
  }

  const sessionUuid = match[1];
  // Find the agent whose session_uuid matches the URL
  const agents = hub?.agents.current() || [];
  const agent = agents.find((a) => a.session_uuid === sessionUuid);
  if (agent) {
    selectedSessionId.value = agent.id;
  } else if (agents.length > 0) {
    // Agents loaded but URL session not found — clear stale selection
    selectedSessionId.value = null;
  }
}
