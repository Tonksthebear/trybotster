// Ambient declarations for the JS dispatcher. Used by the TypeScript-scoped
// ui_contract module to call into the existing hub action layer without
// turning on `allowJs` across the whole project.

export const ACTION: Record<string, string>
export function safeUrl(url: string | null | undefined): string | null
export function dispatch(binding: {
  action: string
  payload?: Record<string, unknown>
}): void
