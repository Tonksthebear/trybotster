import { html } from "htm/preact";

// Inline: horizontal flex container. Same as Stack but flex-row, plus wrap.
// Props: gap (0-6), padding (0-6), align, justify, wrap, children

const GAP = {
  "0": "gap-0", "1": "gap-1", "2": "gap-2", "3": "gap-3", "4": "gap-4", "6": "gap-6", "8": "gap-8",
};

const PADDING = {
  "0": "p-0", "1": "p-1", "2": "p-2", "3": "p-3", "4": "p-4", "6": "p-6", "8": "p-8",
};

const ALIGN = {
  start: "items-start",
  center: "items-center",
  end: "items-end",
  stretch: "items-stretch",
  baseline: "items-baseline",
};

const JUSTIFY = {
  start: "justify-start",
  center: "justify-center",
  end: "justify-end",
  between: "justify-between",
};

export function Inline({ gap = "2", padding, align = "center", justify, wrap, class: cx, children }) {
  const classes = [
    "flex flex-row",
    GAP[gap],
    PADDING[padding],
    ALIGN[align],
    JUSTIFY[justify],
    wrap && "flex-wrap",
    cx,
  ].filter(Boolean).join(" ");

  return html`<div class=${classes}>${children}</div>`;
}
