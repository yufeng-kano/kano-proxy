export function newId(prefix = ""): string {
  const bytes = new Uint8Array(16)
  crypto.getRandomValues(bytes)
  const hex = [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("")
  return prefix ? `${prefix}_${hex}` : hex
}

export function nowIso(): string {
  return new Date().toISOString()
}
