# Class B — unique truecolor style map

- **Killer:** synthetic ≥200 unique `CSI 38;2;…;48;2;…m` styles.
- **Mechanism:** `manualStyleUpdate` → `increaseCapacity(.styles)`.
- **Pin:** single `styles.add`; degrade to default style_id; no log.
