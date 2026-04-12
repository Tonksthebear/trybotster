import { html } from "htm/preact";

// Badge: small label chip.
// Props: text, tone (default/primary/success/warning/danger), size (sm/md)

const TONE = {
  default: "bg-zinc-700/60 text-zinc-300 border-zinc-600/50",
  primary: "bg-primary-500/15 text-primary-300 border-primary-500/30",
  success: "bg-success-500/15 text-success-300 border-success-500/30",
  warning: "bg-warning-500/15 text-warning-300 border-warning-500/30",
  danger: "bg-danger-500/15 text-danger-300 border-danger-500/30",
};

const SIZE = {
  sm: "text-[10px] px-1.5 py-0",
  md: "text-xs px-2 py-0.5",
};

export function Badge({ text, tone = "default", size = "sm", class: cx }) {
  const classes = [
    "inline-flex items-center rounded-full border font-medium leading-snug",
    TONE[tone] || TONE.default,
    SIZE[size] || SIZE.sm,
    cx,
  ].filter(Boolean).join(" ");

  return html`<span class=${classes}>${text}</span>`;
}
