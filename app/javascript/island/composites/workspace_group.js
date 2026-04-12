import { html } from "htm/preact";
import { dispatch, ACTION } from "island/actions";
import { sessionRowProps } from "island/store";
import { SessionRow } from "island/composites/session_row";

// WorkspaceGroupPropsV1:
//   id, title, count, expanded, density, canRename, sessions

export function WorkspaceGroup({
  id,
  title,
  count,
  expanded = true,
  density = "sidebar",
  canRename = true,
  sessions = [],
  hubSlug,
}) {
  const isSidebar = density === "sidebar";

  const handleToggle = () => {
    dispatch({
      action: ACTION.WORKSPACE_TOGGLE,
      payload: { workspaceId: id },
    });
  };

  const handleRenameClick = (e) => {
    e.stopPropagation();
    dispatch({
      action: ACTION.WORKSPACE_RENAME,
      payload: { workspaceId: id, title },
    });
  };

  if (sessions.length === 0) return null;

  const headerCls = isSidebar
    ? "flex items-center gap-1.5 w-full px-2 py-1 text-xs font-semibold text-zinc-400 uppercase tracking-wider hover:text-zinc-200 transition-colors"
    : "flex items-center gap-2 w-full px-4 py-2 text-sm font-semibold text-zinc-300 uppercase tracking-wide hover:text-zinc-100 transition-colors";

  const chevronCls = isSidebar ? "text-[10px]" : "text-xs";

  return html`
    <div class=${isSidebar ? "mb-1" : "mb-3"}>
      <div class="group ${headerCls}">
        <button
          class="flex items-center gap-1.5 min-w-0 flex-1"
          onClick=${handleToggle}
          aria-expanded=${expanded}
        >
          <span
            class="${chevronCls} transition-transform ${expanded ? "" : "-rotate-90"}"
            aria-hidden="true"
          >
            \u25BE
          </span>
          <span class="truncate">${title}</span>
          <span class="text-zinc-600 text-xs ml-1">${count}</span>
        </button>

        ${canRename &&
        html`
          <button
            class="ml-auto opacity-0 group-hover:opacity-100 p-0.5 rounded text-zinc-500 hover:text-zinc-300 transition-all"
            onClick=${handleRenameClick}
            title="Rename workspace"
            aria-label="Rename workspace"
          >
            <svg class="w-3 h-3" viewBox="0 0 16 16" fill="currentColor">
              <path
                d="M12.1 1.3a1.5 1.5 0 012.1 2.1L5.6 12 2 14l2-3.6 8.1-9.1z"
              />
            </svg>
          </button>
        `}
      </div>

      ${expanded &&
      html`
        <div class=${isSidebar ? "mt-0.5" : "mt-1 space-y-1"}>
          ${sessions.map((session) => {
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
