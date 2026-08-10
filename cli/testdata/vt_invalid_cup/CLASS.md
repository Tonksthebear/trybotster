# Class D — invalid multi-param CUP / VT log.warn stack

- **Killer:** after dense ring state, `x.vtlast` is only `56;6H` (completes
  incomplete `\x1b[60;` at ring end into CSI `60;56;6H`).
- **Mechanism:** Ghostty `log.warn("invalid CUP…")` → lib-vt `logFn`/`emitLog`
  EXC_BAD_ACCESS (stack) mid-`vt_write`.
- **Production:** `sess-1786319046-0003-…` (full dump in `vt_crash_hyperlink_map`).
- **Pin:** trybotster/ghostty `logFn` no-ops unless `.err` (`5e9ba17a2`+).
