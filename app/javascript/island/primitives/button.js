import { html } from "htm/preact";
import { Icon } from "island/primitives/icon";

// Button: action trigger.
// Props: label, action (ActionBindingV1: { id, payload, disabled }),
//        variant (solid/ghost), tone, leadingIcon, disabled

function dispatchAction(action) {
  if (!action || action.disabled) return;
  document.dispatchEvent(
    new CustomEvent("botster:action", {
      detail: { id: action.id, payload: action.payload },
      bubbles: true,
    })
  );
}

const VARIANT_TONE = {
  solid: {
    default: "bg-zinc-700 hover:bg-zinc-600 text-zinc-100 border-zinc-600/50",
    primary: "bg-primary-600 hover:bg-primary-500 text-primary-50 border-primary-500/30",
    danger: "bg-danger-600 hover:bg-danger-500 text-danger-50 border-danger-500/30",
    success: "bg-success-600 hover:bg-success-500 text-success-50 border-success-500/30",
  },
  ghost: {
    default: "bg-transparent hover:bg-zinc-800 text-zinc-300 border-transparent",
    primary: "bg-transparent hover:bg-primary-500/10 text-primary-400 border-transparent",
    danger: "bg-transparent hover:bg-danger-500/10 text-danger-400 border-transparent",
    success: "bg-transparent hover:bg-success-500/10 text-success-400 border-transparent",
  },
};

export function Button({ label, action, variant = "solid", tone = "default", leadingIcon, disabled: disabledProp, class: cx }) {
  const isDisabled = disabledProp || action?.disabled;
  const styles = VARIANT_TONE[variant]?.[tone] || VARIANT_TONE.solid.default;

  const classes = [
    "inline-flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-md border transition-colors",
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
    >
      ${leadingIcon && html`<${Icon} name=${leadingIcon} size="sm" />`}
      ${label}
    </button>
  `;
}
