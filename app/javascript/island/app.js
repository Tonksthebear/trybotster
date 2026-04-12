import { html } from "htm/preact";
import {
  sessionOrder,
  sessionsById,
  workspaceOrder,
  workspacesById,
  ungroupedSessionIds,
  selectedSessionId,
  collapsedWorkspaceIds,
  displayName,
  subtext,
  activityState,
  workspaceSessions,
  toggleWorkspaceCollapsed,
} from "island/store";

// Proof-of-life top-level island component.
// Renders workspace groups and ungrouped sessions from the signal store.
// Will be replaced by proper primitives in Wave 2.

function ActivityDot({ state }) {
  if (state === "accessory") return null;
  const symbol = state === "active" ? "\u273A" : "\u25CC";
  const color = state === "active" ? "text-emerald-300" : "text-sky-500";
  return html`<span class="text-xs ${color}" title=${state}>${symbol}</span>`;
}

function SessionRow({ session }) {
  const selected = selectedSessionId.value === session.id;
  const isAccessory = session.session_type === "accessory";
  const name = displayName(session);
  const sub = subtext(session);
  const activity = activityState(session);

  return html`
    <div
      class="flex items-center gap-2 px-3 py-1.5 rounded-md cursor-pointer
             ${selected ? "bg-zinc-700" : "hover:bg-zinc-800"}"
      data-agent-id=${session.id}
    >
      <${ActivityDot} state=${activity} />
      <div class="min-w-0 flex-1">
        <div class="truncate text-sm ${isAccessory ? "text-zinc-400" : "text-zinc-100"}">
          ${name}
        </div>
        ${sub && html`<div class="truncate text-xs text-zinc-500">${sub}</div>`}
      </div>
    </div>
  `;
}

function WorkspaceGroup({ workspaceId }) {
  const ws = workspacesById.value[workspaceId];
  if (!ws) return null;

  const collapsed = collapsedWorkspaceIds.value.has(workspaceId);
  const sessions = workspaceSessions(workspaceId);
  if (sessions.length === 0) return null;

  return html`
    <div class="mb-1">
      <button
        class="flex items-center gap-1.5 w-full px-3 py-1 text-xs font-medium text-zinc-400 uppercase tracking-wide hover:text-zinc-200"
        onClick=${() => toggleWorkspaceCollapsed(workspaceId)}
      >
        <span class="text-[10px] transition-transform ${collapsed ? "-rotate-90" : ""}">
          \u25BE
        </span>
        <span class="truncate">${ws.name || ws.id}</span>
        <span class="text-zinc-600 ml-auto">${sessions.length}</span>
      </button>
      ${!collapsed && sessions.map(
        (s) => html`<${SessionRow} key=${s.id} session=${s} />`,
      )}
    </div>
  `;
}

export function App() {
  const wsOrder = workspaceOrder.value;
  const ungrouped = ungroupedSessionIds.value;
  const byId = sessionsById.value;
  const totalSessions = sessionOrder.value.length;

  if (totalSessions === 0) {
    return html`
      <div class="px-3 py-4 text-sm text-zinc-500">No sessions</div>
    `;
  }

  return html`
    <div>
      ${wsOrder.map(
        (id) => html`<${WorkspaceGroup} key=${id} workspaceId=${id} />`,
      )}
      ${ungrouped.map((id) => {
        const s = byId[id];
        return s ? html`<${SessionRow} key=${id} session=${s} />` : null;
      })}
    </div>
  `;
}
