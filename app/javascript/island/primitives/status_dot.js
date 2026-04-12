import { html } from "htm/preact";

// StatusDot: colored status indicator circle.
// Props: state (neutral/idle/active/success/warning/danger), label

const STATE = {
  neutral: "bg-zinc-500",
  idle: "bg-primary-400",
  active: "bg-success-400",
  success: "bg-success-400",
  warning: "bg-warning-400",
  danger: "bg-danger-400",
};

const PULSE = new Set(["active"]);

export function StatusDot({ state = "neutral", label, class: cx }) {
  const color = STATE[state] || STATE.neutral;
  const pulse = PULSE.has(state);

  const classes = [
    "inline-block size-2 rounded-full shrink-0",
    color,
    pulse && "animate-pulse",
    cx,
  ].filter(Boolean).join(" ");

  return html`
    <span
      class=${classes}
      role="status"
      aria-label=${label || state}
      title=${label || state}
    />
  `;
}
