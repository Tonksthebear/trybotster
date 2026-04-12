import { html } from "htm/preact";

// TreeItem: single item in a tree list.
// Props: id, selected, notification, action (ActionBindingV1),
//        startSlot, title, subtitle, endSlot

function dispatchAction(action) {
  if (!action || action.disabled) return;
  document.dispatchEvent(
    new CustomEvent("botster:action", {
      detail: { id: action.id, payload: action.payload },
      bubbles: true,
    })
  );
}

export function TreeItem({ id, selected, notification, action, startSlot, title, subtitle, endSlot, class: cx }) {
  const rowClasses = [
    "flex items-center gap-2 px-3 py-1.5 rounded-md transition-colors group",
    selected ? "bg-zinc-700" : "hover:bg-zinc-800",
    notification && !selected && "bg-primary-500/5",
    cx,
  ].filter(Boolean).join(" ");

  return html`
    <li data-id=${id} aria-current=${selected ? "true" : undefined}>
      <div class=${rowClasses}>
        <button
          type="button"
          class="flex items-center gap-2 min-w-0 flex-1 text-left outline-none focus-visible:ring-1 focus-visible:ring-primary-500/50 rounded-sm cursor-pointer"
          onClick=${() => dispatchAction(action)}
        >
          ${startSlot}
          <div class="min-w-0 flex-1">
            <div class="flex items-center gap-1.5">
              <span class="truncate text-sm ${selected ? "text-zinc-100" : "text-zinc-300"}">
                ${title}
              </span>
              ${notification && html`
                <span class="size-1.5 rounded-full bg-primary-400 shrink-0" />
              `}
            </div>
            ${subtitle && html`
              <div class="truncate text-xs text-zinc-500">${subtitle}</div>
            `}
          </div>
        </button>
        ${endSlot && html`
          <div class="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 focus-within:opacity-100 transition-opacity">
            ${endSlot}
          </div>
        `}
      </div>
    </li>
  `;
}
