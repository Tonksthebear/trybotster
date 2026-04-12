import { html } from "htm/preact";
import { dispatch, ACTION } from "island/actions";

// HostedPreviewIndicatorPropsV1:
//   status: "inactive" | "starting" | "running" | "error"
//   url: string | null
//   error: string | null
//   installUrl: string | null

const STATUS_CONFIG = {
  inactive: { dot: "bg-zinc-500", label: null },
  starting: { dot: "bg-amber-400 animate-pulse", label: "Starting..." },
  running: { dot: "bg-emerald-400", label: "Preview" },
  error: { dot: "bg-red-400", label: "Error" },
};

export function HostedPreviewIndicator({ status, url, error, installUrl, sessionUuid }) {
  if (!status || status === "inactive") return null;

  const config = STATUS_CONFIG[status] || STATUS_CONFIG.inactive;
  const isClickable = status === "running" && url;

  const dotEl = html`<span class="inline-block w-2 h-2 rounded-full ${config.dot}" />`;

  if (isClickable) {
    return html`
      <button
        class="inline-flex items-center gap-1.5 text-xs text-emerald-400 hover:text-emerald-300 transition-colors"
        onClick=${(e) => {
          e.stopPropagation();
          dispatch({ action: ACTION.PREVIEW_OPEN, payload: { url } });
        }}
        title="Open Cloudflare preview in new tab"
        aria-label="Open Cloudflare preview in new tab"
      >
        ${dotEl}
        <span>${config.label}</span>
      </button>
    `;
  }

  const title =
    status === "starting"
      ? "Cloudflare preview is starting"
      : status === "error"
        ? error || "Cloudflare preview failed"
        : "Cloudflare preview status";

  return html`
    <span
      class="inline-flex items-center gap-1.5 text-xs text-zinc-400"
      title=${title}
    >
      ${dotEl}
      ${config.label && html`<span>${config.label}</span>`}
    </span>
  `;
}
