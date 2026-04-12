import { html } from "htm/preact";

// Panel: surface container with padding, tone, border, radius.
// Props: padding (0-6), tone (default/muted), border, radius, children

const TONE = {
  default: "bg-zinc-900",
  muted: "bg-zinc-900/50",
};

const PADDING = {
  "0": "p-0", "1": "p-1", "2": "p-2", "3": "p-3", "4": "p-4", "6": "p-6", "8": "p-8",
};

const RADIUS = {
  none: "rounded-none",
  sm: "rounded-sm",
  md: "rounded-md",
  lg: "rounded-lg",
  xl: "rounded-xl",
};

export function Panel({ padding = "3", tone = "default", border, radius = "lg", class: cx, children }) {
  const classes = [
    TONE[tone] || TONE.default,
    PADDING[padding],
    RADIUS[radius],
    border && "border border-zinc-700/50",
    cx,
  ].filter(Boolean).join(" ");

  return html`<div class=${classes}>${children}</div>`;
}
