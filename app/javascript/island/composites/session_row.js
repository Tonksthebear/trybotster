import { html } from "htm/preact";
import { dispatch, ACTION } from "island/actions";
import { HostedPreviewIndicator } from "island/composites/hosted_preview_indicator";
import { HostedPreviewError } from "island/composites/hosted_preview_error";
import { SessionActionsMenu } from "island/composites/session_actions_menu";

// SessionRowPropsV1:
//   sessionId, sessionUuid, density,
//   primaryName, titleLine, subtext,
//   selected, notification, sessionType,
//   activityState, hostedPreview, previewError,
//   actionsMenu, canMoveWorkspace, canDelete, inWorktree

function ActivityIndicator({ state }) {
  if (state === "accessory") return null;
  const active = state === "active";
  const cls = active
    ? "bg-emerald-400 shadow-[0_0_6px_rgba(52,211,153,0.5)]"
    : "bg-zinc-500";
  return html`
    <span
      class="shrink-0 w-2 h-2 rounded-full ${cls}"
      title=${state}
      aria-label=${`Session is ${state}`}
    />
  `;
}

export function SessionRow({
  sessionId,
  sessionUuid,
  density = "sidebar",
  primaryName,
  titleLine: title,
  subtext: sub,
  selected = false,
  notification = false,
  sessionType = "agent",
  activityState = "idle",
  hostedPreview,
  previewError,
  actionsMenu,
  canMoveWorkspace,
  canDelete,
  inWorktree,
  hubSlug,
}) {
  const isAccessory = sessionType === "accessory";
  const isSidebar = density === "sidebar";

  // Build session URL for navigation
  const sessionUrl = hubSlug && sessionUuid
    ? `/hubs/${hubSlug}/sessions/${sessionUuid}`
    : null;

  const handleClick = () => {
    dispatch({
      action: ACTION.SESSION_SELECT,
      payload: { sessionId, url: sessionUrl },
    });
  };

  // Density-specific container styles
  const containerCls = isSidebar
    ? `group flex flex-col gap-0 px-2 py-1.5 rounded-md cursor-pointer transition-colors
       ${selected ? "bg-zinc-700/80" : "hover:bg-zinc-800/60"}
       ${notification ? "ring-1 ring-amber-500/40" : ""}`
    : `group flex flex-col gap-1 px-4 py-3 rounded-lg cursor-pointer transition-colors border
       ${selected ? "bg-zinc-700/60 border-zinc-600" : "bg-zinc-800/40 border-zinc-800 hover:bg-zinc-800/70 hover:border-zinc-700"}
       ${notification ? "ring-1 ring-amber-500/40" : ""}`;

  // Text sizes
  const nameCls = isSidebar
    ? `truncate text-sm font-medium ${isAccessory ? "text-zinc-400" : "text-zinc-100"}`
    : `truncate text-base font-medium ${isAccessory ? "text-zinc-400" : "text-zinc-100"}`;

  const subCls = isSidebar ? "text-xs" : "text-sm";

  return html`
    <div>
      <div
        class=${containerCls}
        onClick=${handleClick}
        data-session-id=${sessionId}
        role="button"
        tabindex="0"
        aria-selected=${selected}
        onKeyDown=${(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            handleClick();
          }
        }}
      >
        <div class="flex items-center gap-2 min-w-0">
          <${ActivityIndicator} state=${activityState} />

          <div class="min-w-0 flex-1">
            <div class="flex items-center gap-2">
              <span class=${nameCls}>${primaryName}</span>
              ${hostedPreview &&
              html`
                <${HostedPreviewIndicator}
                  ...${hostedPreview}
                  sessionUuid=${sessionUuid}
                />
              `}
            </div>
            ${title &&
            html`<div class="truncate ${subCls} text-zinc-400">${title}</div>`}
            ${sub &&
            html`<div class="truncate ${subCls} text-zinc-500">${sub}</div>`}
          </div>

          ${actionsMenu &&
          html`
            <${SessionActionsMenu}
              sessionId=${sessionId}
              sessionUuid=${sessionUuid}
              density=${density}
              ...${actionsMenu}
            />
          `}
        </div>
      </div>

      ${previewError &&
      html`
        <${HostedPreviewError}
          error=${previewError}
          installUrl=${hostedPreview?.installUrl}
        />
      `}
    </div>
  `;
}
