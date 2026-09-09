import { describe, expect, it } from "vitest"
import { appendCodexReplayTurn } from "../src/providers/codex_reasoning_cache"
import { codexInputHashes, codexReplayTurn, replayCodexHistory } from "../src/providers/codex_replay_history"

const reasoning = { type: "reasoning", encrypted_content: "opaque-test" }
const body = { instructions: "stable", input: [{ role: "user", content: "task" }] }
const identity = async (id: unknown) => id

describe("Codex turn matching", () => {
  it("rejects tool, instruction, and effort changes even when visible history matches", async () => {
    const hashes = await codexInputHashes(body)
    const turn = (await codexReplayTurn(hashes.at(-1)!, [reasoning], "answer", identity))!
    const input = [...body.input, { role: "assistant", content: [{ type: "output_text", text: "answer" }] }]
    for (const changed of [{ instructions: "edited" }, { tools: [{ type: "function", name: "new" }] }, { reasoning: { effort: "high" } }]) {
      const actual = replayCodexHistory(input, await codexInputHashes({ ...body, ...changed, input }), { turns: [turn] })
      expect(actual.input).toEqual(input)
      expect(actual.history.turns).toEqual([])
    }
  })

  it("matches tool-only calls by arguments and id, not empty assistant text", async () => {
    const hashes = await codexInputHashes(body)
    const call = { type: "function_call", call_id: "one", name: "read", arguments: '{"a":1}' }
    const turn = (await codexReplayTurn(hashes.at(-1)!, [reasoning, call], "", identity))!
    for (const changed of [{ call_id: "two" }, { arguments: '{"a":2}' }, { name: "write" }]) {
      const input = [...body.input, { ...call, ...changed }]
      expect(replayCodexHistory(input, await codexInputHashes({ ...body, input }), { turns: [turn] }).input).toEqual(input)
    }
  })

  it("keeps an older prefix rather than evicting it for an oversized new turn", async () => {
    const hashes = await codexInputHashes(body)
    const turn = (await codexReplayTurn(hashes.at(-1)!, [reasoning], "answer", identity))!
    const history = { turns: [turn] }
    const large = { ...turn, start_hash: "new-start", items: [{ type: "reasoning", encrypted_content: "x".repeat(300_000) }] }
    expect(appendCodexReplayTurn(history, large)).toBe(history)
    expect(appendCodexReplayTurn(history, null)).toBe(history)
    expect(appendCodexReplayTurn(history, turn)).toBe(history)
  })
})
