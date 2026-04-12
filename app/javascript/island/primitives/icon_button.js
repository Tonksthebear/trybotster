import { html } from "htm/preact";
import { Icon } from "island/primitives/icon";

// IconButton: icon-only action trigger.
// Props: icon (heroicon name), label (accessibility), action (ActionBindingV1),
//        tone, disabled

function dispatchAction(action) {
  if (!action || action.disabled) return;
  document.dispatchEvent(
    new CustomEvent("botster:action", {
      detail: { id: action.id, payload: action.payload },
      bubbles: true,
    })
  );
}

const TONE = {
  default: "text-zinc-400 hover:text-zinc-200 hover:bg-zinc-700/50",
  primary: "text-primary-400 hover:text-primary-300 hover:bg-primary-500/10",
  danger: "text-danger-400 hover:text-danger-300 hover:bg-danger-500/10",
  success: "text-success-400 hover:text-success-300 hover:bg-success-500/10",
};

export function IconButton({ icon, label, action, tone = "default", disabled: disabledProp, class: cx }) {
  const isDisabled = disabledProp || action?.disabled;
  const styles = TONE[tone] || TONE.default;

  const classes = [
    "inline-flex items-center justify-center size-7 rounded-md transition-colors",
    styles,
    isDisabled && "opacity-40 pointer-events-none",
    cx,
  ].filter(Boolean).join(" ");

  return html`
    <button
      type="button"
      class=${classes}
      disabled=${isDisabled}
      onClick=${() => dispatchAction(action)}
      aria-label=${label}
      title=${label}
    >
      <${Icon} name=${icon} size="sm" />
    </button>
  `;
}
