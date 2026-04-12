import { html } from "htm/preact";
import { useRef, useState, useEffect, useCallback } from "preact/hooks";
import { dispatch, ACTION } from "island/actions";

// SessionActionsMenuPropsV1:
//   sessionId, sessionUuid
//   canPreview: boolean
//   previewStatus: "inactive" | "starting" | "running" | "error"
//   previewUrl: string | null
//   canMove: boolean
//   canDelete: boolean
//   density: "sidebar" | "panel"

export function SessionActionsMenu({
  sessionId,
  sessionUuid,
  canPreview,
  previewStatus,
  previewUrl,
  canMove,
  canDelete,
  density,
}) {
  const [open, setOpen] = useState(false);
  const menuRef = useRef(null);
  const triggerRef = useRef(null);

  // Close on outside click
  useEffect(() => {
    if (!open) return;
    function handleClick(e) {
      if (menuRef.current && !menuRef.current.contains(e.target)) {
        setOpen(false);
      }
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [open]);

  // Close on Escape
  useEffect(() => {
    if (!open) return;
    function handleKey(e) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("keydown", handleKey);
    return () => document.removeEventListener("keydown", handleKey);
  }, [open]);

  const menuItem = useCallback(
    (label, action, payload, { disabled = false, destructive = false } = {}) => {
      const base =
        "w-full text-left px-3 py-1.5 text-sm transition-colors";
      const colors = destructive
        ? "text-red-400 hover:bg-red-950/50 hover:text-red-300"
        : "text-zinc-300 hover:bg-zinc-700 hover:text-zinc-100";
      const disabledCls = disabled
        ? "opacity-40 cursor-not-allowed"
        : "cursor-pointer";

      return html`
        <button
          class="${base} ${colors} ${disabledCls}"
          disabled=${disabled}
          onClick=${(e) => {
            e.stopPropagation();
            if (!disabled) {
              dispatch({ action, payload });
              setOpen(false);
            }
          }}
        >
          ${label}
        </button>
      `;
    },
    [],
  );

  const previewToggleLabel =
    previewStatus === "running" || previewStatus === "starting"
      ? "Disable preview"
      : "Enable preview";

  const previewReady = previewStatus === "running" && previewUrl;

  // Sidebar: render trigger as icon-only, panel: text button
  const triggerCls =
    density === "sidebar"
      ? "opacity-0 group-hover:opacity-100 focus:opacity-100 p-1 rounded text-zinc-400 hover:text-zinc-200 hover:bg-zinc-700 transition-all"
      : "p-1 rounded text-zinc-400 hover:text-zinc-200 hover:bg-zinc-700 transition-colors";

  return html`
    <div class="relative" ref=${menuRef}>
      <button
        ref=${triggerRef}
        class=${triggerCls}
        onClick=${(e) => {
          e.stopPropagation();
          setOpen(!open);
        }}
        aria-haspopup="true"
        aria-expanded=${open}
        title="Session actions"
      >
        <svg class="w-4 h-4" viewBox="0 0 16 16" fill="currentColor">
          <circle cx="8" cy="3" r="1.5" />
          <circle cx="8" cy="8" r="1.5" />
          <circle cx="8" cy="13" r="1.5" />
        </svg>
      </button>

      ${open &&
      html`
        <div
          class="absolute right-0 top-full mt-1 w-48 py-1 rounded-lg bg-zinc-800 border border-zinc-700 shadow-xl z-50"
          role="menu"
        >
          ${canPreview &&
          html`
            ${menuItem(previewToggleLabel, ACTION.PREVIEW_TOGGLE, {
              sessionUuid,
            })}
            ${previewReady &&
            menuItem("Open preview", ACTION.PREVIEW_OPEN, {
              url: previewUrl,
            })}
            <div class="my-1 border-t border-zinc-700" />
          `}
          ${canMove &&
          menuItem("Move to workspace...", ACTION.SESSION_MOVE, {
            sessionId,
            sessionUuid,
          })}
          ${canDelete &&
          html`
            <div class="my-1 border-t border-zinc-700" />
            ${menuItem(
              "Delete session",
              ACTION.SESSION_DELETE,
              { sessionId, sessionUuid },
              { destructive: true },
            )}
          `}
        </div>
      `}
    </div>
  `;
}
