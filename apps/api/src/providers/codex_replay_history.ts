/** Match opaque reasoning to the exact visible assistant turn that produced it. */
import {
  hashAssistantText,
  type CodexReasoningReplayEntry,
  type CodexReasoningReplayItem,
  type CodexReasoningReplayTurn,
} from "./codex_reasoning_cache"

type Item = Record<string, unknown>

function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`
  if (value && typeof value === "object") {
    return `{${Object.entries(value).filter(([, v]) => v !== undefined)
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([k, v]) => `${JSON.stringify(k)}:${canonical(v)}`).join(",")}}`
  }
  return JSON.stringify(value) ?? "null"
}

function visibleItem(item: Item): Item {
  if (item.type !== "function_call") return item
  let args = item.arguments
  if (typeof args === "string") {
    try { args = JSON.parse(args) } catch { /* Compare invalid JSON literally. */ }
  }
  // Response-only ids/status must not distinguish an echoed tool call.
  return { type: item.type, call_id: item.call_id, name: item.name, arguments: args }
}

async function extendHash(previous: string, item: Item): Promise<string> {
  return hashAssistantText(`${previous}\n${canonical(visibleItem(item))}`)
}

export async function codexInputHashes(body: Record<string, unknown>): Promise<string[]> {
  // Cache matching includes the stable request configuration, never session secrets.
  const hashes = [await hashAssistantText(canonical({
    instructions: body.instructions, tools: body.tools, tool_choice: body.tool_choice,
    reasoning: body.reasoning, text: body.text,
  }))]
  for (const item of body.input as Item[]) {
    hashes.push(await extendHash(hashes[hashes.length - 1]!, item))
  }
  return hashes
}

export async function codexReplayTurn(
  startHash: string,
  items: CodexReasoningReplayItem[],
  assistantText: string,
  shortenCallId: (id: unknown) => Promise<unknown>,
): Promise<CodexReasoningReplayTurn | null> {
  if (!items.some((item) => item.type === "reasoning")) return null
  // Custom tools cannot round-trip through our Chat conversion. Do not guess.
  if (items.some((item) => item.type === "custom_tool_call")) return null
  const calls: Item[] = []
  for (const item of items.filter((item) => item.type === "function_call")) {
    calls.push({ type: "function_call", call_id: await shortenCallId(item.call_id),
      name: item.name, arguments: item.arguments ?? "{}" })
  }
  const visible: Item[] = assistantText
    ? [{ role: "assistant", content: [{ type: "output_text", text: assistantText }] }, ...calls]
    : calls
  if (visible.length === 0) return null
  let endHash = startHash
  for (const item of visible) endHash = await extendHash(endHash, item)
  return {
    start_hash: startHash, end_hash: endHash, visible_count: visible.length,
    items: [...items.filter((item) => item.type === "reasoning"), ...calls] as CodexReasoningReplayItem[],
  }
}

export function replayCodexHistory(
  input: Item[],
  hashes: string[],
  history: CodexReasoningReplayEntry | null,
): { input: Item[]; history: CodexReasoningReplayEntry } {
  const positions = new Map(hashes.map((hash, index) => [hash, index]))
  const matches = new Map<number, CodexReasoningReplayTurn>()
  for (const turn of history?.turns ?? []) {
    const start = positions.get(turn.start_hash)
    if (start === undefined || hashes[start + turn.visible_count] !== turn.end_hash) continue
    if (input.slice(start, start + turn.visible_count).some((item) => item.type === "reasoning")) continue
    matches.set(start, turn)
  }
  const output: Item[] = []
  const retained: CodexReasoningReplayTurn[] = []
  for (let index = 0; index < input.length; index++) {
    const turn = matches.get(index)
    if (!turn) { output.push(input[index]!); continue }
    retained.push(turn)
    output.push(...turn.items.filter((item) => item.type === "reasoning"))
    const calls = new Map(turn.items.filter((item) => item.type === "function_call")
      .map((item) => [item.call_id, item]))
    // Restore original argument serialization, without duplicating tool calls.
    for (const item of input.slice(index, index + turn.visible_count)) {
      output.push(item.type === "function_call" ? calls.get(item.call_id) ?? item : item)
    }
    index += turn.visible_count - 1
  }
  return { input: output, history: { turns: retained } }
}
