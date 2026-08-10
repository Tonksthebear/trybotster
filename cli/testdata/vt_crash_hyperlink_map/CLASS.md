# Classes C + D — production dump sess-1786319046

- **Full capture** that died signal 11 / exit 139 on a post–style-pin hub.
- **C:** `print` → `cursorSetHyperlink` → `increaseCapacity(.hyperlink_bytes)`.
- **D:** invalid multi-param CUP after mid-CSI → `log.warn` → emitLog stack
  (minimal form: `vt_invalid_cup`).
- Prefer both this fixture and `vt_invalid_cup` green.
