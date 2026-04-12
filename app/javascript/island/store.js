import { signal, computed, batch } from "@preact/signals";

// --- Normalized store (AgentWorkspaceStoreV1 shape) ---

export const sessionsById = signal({});
export const sessionOrder = signal([]);
export const workspacesById = signal({});
export const workspaceOrder = signal([]);
export const ungroupedSessionIds = signal([]);
export const selectedSessionId = signal(null);
export const collapsedWorkspaceIds = signal(new Set());
export const surface = signal("agent_list");

// --- Normalizer ---
// Takes the raw hub payload (agents array + workspaces array) and updates the store.
// This is the single entry point for all state mutations from the hub.

export function normalize(agents, workspaces) {
  batch(() => {
    // Normalize sessions
    const byId = {};
    const order = [];
    for (const agent of agents) {
      byId[agent.id] = agent;
      order.push(agent.id);
    }
    sessionsById.value = byId;
    sessionOrder.value = order;

    // Normalize workspaces
    const wsById = {};
    const wsOrder = [];
    const grouped = new Set();

    for (const ws of workspaces) {
      wsById[ws.id] = ws;
      wsOrder.push(ws.id);
      if (Array.isArray(ws.agents)) {
        for (const id of ws.agents) grouped.add(id);
      }
    }
    workspacesById.value = wsById;
    workspaceOrder.value = wsOrder;

    // Ungrouped = sessions not in any workspace
    ungroupedSessionIds.value = order.filter((id) => !grouped.has(id));
  });
}

// --- Selectors ---

// Display name: label > display_name > id
export function displayName(session) {
  if (!session) return "";
  const label = session.label?.trim();
  if (label) return label;
  return session.display_name || session.id;
}

// Subtext: target_name + branch_name + config_name
export function subtext(session) {
  if (!session) return "";
  const parts = [];
  if (session.target_name) parts.push(session.target_name);
  if (session.branch_name) parts.push(session.branch_name);
  const configName = session.agent_name || session.profile_name;
  if (configName) parts.push(configName);
  if (session.session_type === "accessory" && parts.length === 0) {
    parts.push("accessory");
  }
  return parts.join(" \u00b7 ");
}

// Title line: title + task when different from display name
export function titleLine(session) {
  if (!session) return "";
  const parts = [];
  const title = session.title?.trim();
  const primary = displayName(session);
  if (title && title !== primary) parts.push(title);
  if (session.task) parts.push(session.task);
  return parts.join(" \u00b7 ");
}

// Activity state for non-accessory sessions
export function activityState(session) {
  if (!session) return "idle";
  if (session.session_type === "accessory") return "accessory";
  return session.is_idle !== false ? "idle" : "active";
}

// Preview affordances
export function previewState(session) {
  if (!session) return { canPreview: false };
  const hp = session.hosted_preview;
  return {
    canPreview: !!session.port,
    status: hp?.status || "inactive",
    url: typeof hp?.url === "string" ? hp.url : null,
    error: hp?.error || null,
    installUrl: typeof hp?.install_url === "string" ? hp.install_url : null,
  };
}

// Ordered sessions for a given workspace
export function workspaceSessions(workspaceId) {
  const ws = workspacesById.value[workspaceId];
  if (!ws || !Array.isArray(ws.agents)) return [];
  const byId = sessionsById.value;
  return ws.agents.map((id) => byId[id]).filter(Boolean);
}

// Ungrouped sessions
export function getUngroupedSessions() {
  const byId = sessionsById.value;
  return ungroupedSessionIds.value.map((id) => byId[id]).filter(Boolean);
}

// Toggle workspace collapse
export function toggleWorkspaceCollapsed(workspaceId) {
  const next = new Set(collapsedWorkspaceIds.value);
  if (next.has(workspaceId)) {
    next.delete(workspaceId);
  } else {
    next.add(workspaceId);
  }
  collapsedWorkspaceIds.value = next;
}
