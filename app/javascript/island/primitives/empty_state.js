import { html } from "htm/preact";
import { Icon } from "island/primitives/icon";
import { Text } from "island/primitives/text";
import { Button } from "island/primitives/button";

// EmptyState: placeholder for empty lists/panels.
// Props: title, description, icon (heroicon name), primaryAction (ActionBindingV1: { id, payload, label, disabled })

export function EmptyState({ title, description, icon, primaryAction, class: cx }) {
  const classes = [
    "flex flex-col items-center justify-center gap-3 py-8 px-4 text-center",
    cx,
  ].filter(Boolean).join(" ");

  return html`
    <div class=${classes}>
      ${icon && html`<${Icon} name=${icon} size="lg" tone="muted" />`}
      ${title && html`<${Text} text=${title} size="sm" tone="muted" weight="medium" />`}
      ${description && html`<${Text} text=${description} size="xs" tone="faint" />`}
      ${primaryAction && html`
        <${Button}
          label=${primaryAction.label}
          action=${{ id: primaryAction.id, payload: primaryAction.payload, disabled: primaryAction.disabled }}
          variant="ghost"
          tone="primary"
          class="mt-1"
        />
      `}
    </div>
  `;
}
