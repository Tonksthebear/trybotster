import { html } from "htm/preact";
import {
  workspaceOrder,
  workspacesById,
  sessionOrder,
  surface,
  workspaceGroupProps,
  getUngroupedSessions,
} from "island/store";
import { WorkspaceList } from "island/composites/workspace_list";

// Derive hub slug from current URL: /hubs/:slug/...
function hubSlugFromUrl() {
  const match = window.location.pathname.match(/\/hubs\/([^/]+)/);
  return match ? match[1] : null;
}

export function App() {
  const wsOrder = workspaceOrder.value;
  const totalSessions = sessionOrder.value.length;
  const density = surface.value === "sidebar" ? "sidebar" : "panel";
  const hubSlug = hubSlugFromUrl();

  // Derive workspace group props
  const groups = wsOrder
    .map((id) => workspaceGroupProps(id))
    .filter(Boolean);

  // Ungrouped sessions (raw objects — WorkspaceList will derive row props)
  const ungroupedSessions = getUngroupedSessions();

  const emptyState = totalSessions === 0 ? "No sessions" : null;

  return html`
    <${WorkspaceList}
      density=${density}
      groups=${groups}
      ungroupedSessions=${ungroupedSessions}
      emptyState=${emptyState}
      hubSlug=${hubSlug}
    />
  `;
}
