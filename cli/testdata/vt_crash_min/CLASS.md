# Class A — OSC-8 startHyperlink capacity

- **Killer:** long unique OSC-8 hyperlink streams.
- **Mechanism:** `startHyperlink` → `increaseCapacity` stack SEGV.
- **Pin:** silent hyperlink degrade on page pressure; no log on that path.
