import { html } from "htm/preact";
import { WorkspaceGroup } from "island/composites/workspace_group";
import { SessionRow } from "island/composites/session_row";
import { sessionRowProps } from "island/store";

// WorkspaceListPropsV1:
//   density: "sidebar" | "panel"
//   groups: WorkspaceGroupPropsV1[]
//   ungroupedSessions: session[]
//   emptyState: string | null
//   hubSlug: string

export function WorkspaceList({
  density = "sidebar",
  groups = [],
  ungroupedSessions = [],
  emptyState = null,
  hubSlug,
}) {
  const hasContent = groups.length > 0 || ungroupedSessions.length > 0;

  if (!hasContent) {
    return html`
      <div class="px-3 py-6 text-center">
        <p class="text-sm text-zinc-500">
          ${emptyState || "No sessions"}
        </p>
      </div>
    `;
  }

  const isSidebar = density === "sidebar";

  return html`
    <div class=${isSidebar ? "py-1" : "py-2 space-y-2"}>
      ${groups.map(
        (group) =>
          html`<${WorkspaceGroup}
            key=${group.id}
            ...${group}
            density=${density}
            hubSlug=${hubSlug}
          />`,
      )}
      ${ungroupedSessions.length > 0 &&
      html`
        <div class=${isSidebar ? "" : "space-y-1"}>
          ${ungroupedSessions.map((session) => {
            const props = sessionRowProps(session);
            return props
              ? html`<${SessionRow}
                  key=${session.id}
                  ...${props}
                  hubSlug=${hubSlug}
                />`
              : null;
          })}
        </div>
      `}
    </div>
  `;
}
