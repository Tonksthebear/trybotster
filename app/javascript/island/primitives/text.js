import { html } from "htm/preact";

// Text: styled text span.
// Props: text, size (xs/sm/md), tone, weight, italic, truncate, monospace

const SIZE = {
  xs: "text-xs",
  sm: "text-sm",
  md: "text-base",
};

const TONE = {
  default: "text-zinc-100",
  muted: "text-zinc-400",
  faint: "text-zinc-500",
  primary: "text-primary-400",
  success: "text-success-400",
  warning: "text-warning-400",
  danger: "text-danger-400",
};

const WEIGHT = {
  normal: "font-normal",
  medium: "font-medium",
  semibold: "font-semibold",
  bold: "font-bold",
};

export function Text({ text, size = "sm", tone = "default", weight, italic, truncate, monospace, class: cx, children }) {
  const classes = [
    SIZE[size] || SIZE.sm,
    TONE[tone] || TONE.default,
    weight && (WEIGHT[weight] || weight),
    italic && "italic",
    truncate && "truncate",
    monospace && "font-mono",
    cx,
  ].filter(Boolean).join(" ");

  return html`<span class=${classes}>${text || children}</span>`;
}
