import { html } from "htm/preact";

// ScrollArea: overflow container.
// Props: axis (y/x/both), class, children

const OVERFLOW = {
  y: "overflow-y-auto overflow-x-hidden",
  x: "overflow-x-auto overflow-y-hidden",
  both: "overflow-auto",
};

export function ScrollArea({ axis = "y", class: cx, children }) {
  const classes = [
    OVERFLOW[axis] || OVERFLOW.y,
    "min-h-0",
    cx,
  ].filter(Boolean).join(" ");

  return html`<div class=${classes}>${children}</div>`;
}
