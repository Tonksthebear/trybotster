import { html } from "htm/preact";

// Tree: container for tree items with density control.
// Props: density (compact/default/relaxed), children

const DENSITY = {
  compact: "gap-0",
  default: "gap-0.5",
  relaxed: "gap-1",
};

export function Tree({ density = "default", class: cx, children }) {
  const classes = [
    "flex flex-col list-none m-0 p-0",
    DENSITY[density] || DENSITY.default,
    cx,
  ].filter(Boolean).join(" ");

  return html`<ul class=${classes}>${children}</ul>`;
}
