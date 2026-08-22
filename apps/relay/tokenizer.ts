/**
 * o200k_base token counting for `POST /count-tokens` (docs/codex-relay.md
 * § Token counting). Lives in its own module so relay.ts can load it lazily:
 * the ranks are a multi-MB parse, and a codex stream cold-start must never
 * pay for them — only the first count on an instance does.
 */

import { Tiktoken } from "js-tiktoken/lite"
import o200k_base from "js-tiktoken/ranks/o200k_base"

const encoder = new Tiktoken(o200k_base)

export function countTokens(texts: string[]): number {
  let total = 0
  for (const text of texts) total += encoder.encode(text).length
  return total
}
