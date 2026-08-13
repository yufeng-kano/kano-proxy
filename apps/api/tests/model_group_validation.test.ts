import { describe, expect, it } from "vitest"
import {
  MAX_GROUP_NAME_LENGTH,
  MAX_MODEL_GROUPS_PER_USER,
  MAX_TARGETS_PER_GROUP,
  validateGroupName,
  validateGroupTargets,
} from "../src/utils/model_group"
import { parseGroupTargets } from "../src/db/model_groups"

describe("validateGroupName", () => {
  it("accepts a plain bare name", () => {
    expect(validateGroupName("opus")).toBeNull()
  })

  it("accepts the maximum length (128 chars)", () => {
    expect(validateGroupName("a".repeat(MAX_GROUP_NAME_LENGTH))).toBeNull()
  })

  it("rejects empty", () => {
    expect(validateGroupName("")).not.toBeNull()
  })

  it("rejects above maximum length", () => {
    expect(validateGroupName("a".repeat(MAX_GROUP_NAME_LENGTH + 1))).not.toBeNull()
  })

  it("rejects internal whitespace", () => {
    expect(validateGroupName("my group")).not.toBeNull()
  })

  it("rejects a leading/trailing space (caller is expected to trim first, but this still must reject)", () => {
    expect(validateGroupName(" opus")).not.toBeNull()
    expect(validateGroupName("opus ")).not.toBeNull()
  })

  it("rejects any '/'", () => {
    expect(validateGroupName("claude-code/opus")).not.toBeNull()
    expect(validateGroupName("a/b")).not.toBeNull()
  })

  it("accepts punctuation other than whitespace and '/'", () => {
    expect(validateGroupName("gpt-4o")).toBeNull()
    expect(validateGroupName("my_group.v2")).toBeNull()
  })
})

describe("validateGroupTargets", () => {
  const builtinOnly = (prefix: string) => prefix === "claude-code" || prefix === "grok" || prefix === "codex"

  it("accepts a single valid target", () => {
    const res = validateGroupTargets(["claude-code/claude-opus-5"], builtinOnly)
    expect(res).toEqual({ ok: true, targets: ["claude-code/claude-opus-5"] })
  })

  it("accepts up to the max target count", () => {
    const targets = Array.from({ length: MAX_TARGETS_PER_GROUP }, (_, i) => `claude-code/model-${i}`)
    const res = validateGroupTargets(targets, builtinOnly)
    expect(res.ok).toBe(true)
  })

  it("rejects an empty array", () => {
    const res = validateGroupTargets([], builtinOnly)
    expect(res.ok).toBe(false)
  })

  it("rejects a non-array", () => {
    const res = validateGroupTargets("claude-code/claude-opus-5", builtinOnly)
    expect(res.ok).toBe(false)
  })

  it("rejects more than the max target count", () => {
    const targets = Array.from({ length: MAX_TARGETS_PER_GROUP + 1 }, (_, i) => `claude-code/model-${i}`)
    const res = validateGroupTargets(targets, builtinOnly)
    expect(res.ok).toBe(false)
  })

  it("rejects a target with an unknown/unresolvable provider prefix", () => {
    const res = validateGroupTargets(["not-a-real-provider/model"], builtinOnly)
    expect(res.ok).toBe(false)
  })

  it("rejects a bare-name target (no '/') — a group can never target another group", () => {
    const res = validateGroupTargets(["some-other-group"], builtinOnly)
    expect(res.ok).toBe(false)
  })

  it("rejects duplicate targets within one group", () => {
    const res = validateGroupTargets(
      ["claude-code/claude-opus-5", "claude-code/claude-opus-5"],
      builtinOnly,
    )
    expect(res.ok).toBe(false)
  })

  it("rejects a non-string entry", () => {
    const res = validateGroupTargets([42], builtinOnly)
    expect(res.ok).toBe(false)
  })

  it("accepts a custom provider slug prefix when the resolver allows it", () => {
    const resolvePrefix = (prefix: string) => prefix === "my-endpoint"
    const res = validateGroupTargets(["my-endpoint/gpt-4o"], resolvePrefix)
    expect(res).toEqual({ ok: true, targets: ["my-endpoint/gpt-4o"] })
  })

  it("trims each target string", () => {
    const res = validateGroupTargets([" claude-code/claude-opus-5 "], builtinOnly)
    expect(res.ok).toBe(true)
    if (res.ok) expect(res.targets).toEqual(["claude-code/claude-opus-5"])
  })
})

describe("parseGroupTargets — tolerant parse (docs/database.md model_groups.targets_json)", () => {
  it("returns [] for null", () => {
    expect(parseGroupTargets(null)).toEqual([])
  })

  it("parses a stored JSON array of plain strings", () => {
    expect(parseGroupTargets('["claude-code/claude-opus-5","grok/grok-4.5"]')).toEqual([
      "claude-code/claude-opus-5",
      "grok/grok-4.5",
    ])
  })

  it("returns [] for malformed JSON", () => {
    expect(parseGroupTargets("not json")).toEqual([])
  })

  it("returns [] for a JSON value that isn't an array", () => {
    expect(parseGroupTargets('{"a":1}')).toEqual([])
  })

  it("tolerates a future per-target object shape, reading its 'model' field", () => {
    expect(parseGroupTargets('[{"model":"claude-code/claude-opus-5","weight":2}]')).toEqual([
      "claude-code/claude-opus-5",
    ])
  })

  it("mixes plain strings and object entries in one array", () => {
    expect(
      parseGroupTargets('["grok/grok-4.5",{"model":"codex/gpt-5.2"}]'),
    ).toEqual(["grok/grok-4.5", "codex/gpt-5.2"])
  })

  it("drops an entry that is neither a string nor an object with a string 'model' field", () => {
    expect(parseGroupTargets('[42, {"notModel":"x"}, "grok/grok-4.5"]')).toEqual(["grok/grok-4.5"])
  })
})

describe("limits", () => {
  it("MAX_MODEL_GROUPS_PER_USER is 50 per docs/providers.md", () => {
    expect(MAX_MODEL_GROUPS_PER_USER).toBe(50)
  })

  it("MAX_TARGETS_PER_GROUP is 20 per docs/providers.md", () => {
    expect(MAX_TARGETS_PER_GROUP).toBe(20)
  })
})
