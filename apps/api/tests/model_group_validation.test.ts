import { describe, expect, it } from "vitest"
import {
  MAX_DISPLAY_NAME_LENGTH,
  MAX_MODEL_GROUPS_PER_USER,
  MAX_MODEL_NAME_LENGTH,
  MAX_MODELS_PER_GROUP,
  MAX_TARGETS_PER_MODEL,
  validateDisplayName,
  validateGroupModels,
  validateGroupSlug,
  validateGroupTargets,
  validateModelName,
} from "../src/utils/model_group"
import { parseGroupTargets } from "../src/db/model_groups"

describe("validateModelName", () => {
  it("accepts a plain bare name", () => {
    expect(validateModelName("opus")).toBeNull()
  })

  it("accepts the maximum length (128 chars)", () => {
    expect(validateModelName("a".repeat(MAX_MODEL_NAME_LENGTH))).toBeNull()
  })

  it("rejects empty", () => {
    expect(validateModelName("")).not.toBeNull()
  })

  it("rejects above maximum length", () => {
    expect(validateModelName("a".repeat(MAX_MODEL_NAME_LENGTH + 1))).not.toBeNull()
  })

  it("rejects internal whitespace", () => {
    expect(validateModelName("my model")).not.toBeNull()
  })

  it("rejects a leading/trailing space (caller is expected to trim first, but this still must reject)", () => {
    expect(validateModelName(" opus")).not.toBeNull()
    expect(validateModelName("opus ")).not.toBeNull()
  })

  it("accepts '/' — a group endpoint has no provider/model resolution to collide with", () => {
    expect(validateModelName("claude-code/claude-opus-5")).toBeNull()
    expect(validateModelName("a/b")).toBeNull()
  })

  it("accepts punctuation other than whitespace", () => {
    expect(validateModelName("gpt-4o")).toBeNull()
    expect(validateModelName("my_group.v2")).toBeNull()
  })
})

describe("validateGroupSlug", () => {
  it("accepts a plain slug", () => {
    expect(validateGroupSlug("my-tools")).toBeNull()
  })

  it("rejects too short, too long, and bad shapes", () => {
    expect(validateGroupSlug("a")).not.toBeNull()
    expect(validateGroupSlug("a".repeat(33))).not.toBeNull()
    expect(validateGroupSlug("-lead")).not.toBeNull()
    expect(validateGroupSlug("trail-")).not.toBeNull()
    expect(validateGroupSlug("UPPER")).not.toBeNull()
    expect(validateGroupSlug("has space")).not.toBeNull()
  })

  it("has no reserved-word list — /g/ is its own namespace", () => {
    expect(validateGroupSlug("openai")).toBeNull()
    expect(validateGroupSlug("api")).toBeNull()
    expect(validateGroupSlug("claude-code")).toBeNull()
  })
})

describe("validateDisplayName", () => {
  it("accepts a plain name", () => {
    expect(validateDisplayName("Opus")).toBeNull()
  })

  it("accepts free text with spaces — a label, not a callable id", () => {
    expect(validateDisplayName("OpenAI GPT-4o family")).toBeNull()
  })

  it("accepts the maximum length (64 chars)", () => {
    expect(validateDisplayName("a".repeat(MAX_DISPLAY_NAME_LENGTH))).toBeNull()
  })

  it("rejects empty", () => {
    expect(validateDisplayName("")).not.toBeNull()
  })

  it("rejects above maximum length", () => {
    expect(validateDisplayName("a".repeat(MAX_DISPLAY_NAME_LENGTH + 1))).not.toBeNull()
  })

  it("allows '/' and other punctuation — free text, not a callable id", () => {
    expect(validateDisplayName("GPT-4o / GPT-4 family")).toBeNull()
  })
})

describe("validateGroupModels", () => {
  const builtinOnly = (prefix: string) => prefix === "claude-code" || prefix === "grok"
  const noAccounts = async () => false

  it("accepts a single model with targets, trimming the name", async () => {
    const res = await validateGroupModels(
      [{ name: " gpt-4o ", targets: ["claude-code/claude-opus-5"] }],
      builtinOnly,
      noAccounts,
    )
    expect(res).toEqual({
      ok: true,
      models: [{ name: "gpt-4o", targets: [{ model: "claude-code/claude-opus-5", account_id: null }] }],
    })
  })

  it("accepts up to the max model count", async () => {
    const models = Array.from({ length: MAX_MODELS_PER_GROUP }, (_, i) => ({
      name: `model-${i}`,
      targets: ["grok/grok-4.5"],
    }))
    const res = await validateGroupModels(models, builtinOnly, noAccounts)
    expect(res.ok).toBe(true)
  })

  it("rejects an empty array, a non-array, and above-max counts", async () => {
    expect((await validateGroupModels([], builtinOnly, noAccounts)).ok).toBe(false)
    expect((await validateGroupModels("gpt-4o", builtinOnly, noAccounts)).ok).toBe(false)
    const models = Array.from({ length: MAX_MODELS_PER_GROUP + 1 }, (_, i) => ({
      name: `model-${i}`,
      targets: ["grok/grok-4.5"],
    }))
    expect((await validateGroupModels(models, builtinOnly, noAccounts)).ok).toBe(false)
  })

  it("rejects a non-object entry and a non-string name", async () => {
    expect((await validateGroupModels(["gpt-4o"], builtinOnly, noAccounts)).ok).toBe(false)
    expect(
      (await validateGroupModels([{ name: 42, targets: ["grok/grok-4.5"] }], builtinOnly, noAccounts)).ok,
    ).toBe(false)
  })

  it("rejects an in-payload duplicate name (trimmed first; case still distinguishes)", async () => {
    const dup = await validateGroupModels(
      [
        { name: "gpt-4o", targets: ["grok/grok-4.5"] },
        { name: " gpt-4o ", targets: ["claude-code/claude-opus-5"] },
      ],
      builtinOnly,
      noAccounts,
    )
    expect(dup.ok).toBe(false)
    const cased = await validateGroupModels(
      [
        { name: "GPT-4o", targets: ["grok/grok-4.5"] },
        { name: "gpt-4o", targets: ["claude-code/claude-opus-5"] },
      ],
      builtinOnly,
      noAccounts,
    )
    expect(cased.ok).toBe(true)
  })

  it("a model's target error is prefixed with the model name", async () => {
    const res = await validateGroupModels([{ name: "gpt-4o", targets: [] }], builtinOnly, noAccounts)
    expect(res.ok).toBe(false)
    if (!res.ok) expect(res.error).toContain('model "gpt-4o"')
  })

  it("a slash-carrying name is legal and can mirror a full provider/model id", async () => {
    const res = await validateGroupModels(
      [{ name: "claude-code/claude-opus-5", targets: ["grok/grok-4.5"] }],
      builtinOnly,
      noAccounts,
    )
    expect(res.ok).toBe(true)
  })
})

describe("validateGroupTargets", () => {
  const builtinOnly = (prefix: string) => prefix === "claude-code" || prefix === "grok" || prefix === "codex"
  /** No target in these tests pins an account unless a test overrides this. */
  const noAccounts = async () => false
  const allAccounts = async () => true

  it("accepts a single valid string-shorthand target (v3.0.0 wire shape) — normalizes to {model, account_id: null}", async () => {
    const res = await validateGroupTargets(["claude-code/claude-opus-5"], builtinOnly, noAccounts)
    expect(res).toEqual({ ok: true, targets: [{ model: "claude-code/claude-opus-5", account_id: null }] })
  })

  it("accepts a bare {model} object with the same result as the string shorthand", async () => {
    const res = await validateGroupTargets(
      [{ model: "claude-code/claude-opus-5" }],
      builtinOnly,
      noAccounts,
    )
    expect(res).toEqual({ ok: true, targets: [{ model: "claude-code/claude-opus-5", account_id: null }] })
  })

  it("accepts up to the max target count", async () => {
    const targets = Array.from({ length: MAX_TARGETS_PER_MODEL }, (_, i) => `claude-code/model-${i}`)
    const res = await validateGroupTargets(targets, builtinOnly, noAccounts)
    expect(res.ok).toBe(true)
  })

  it("rejects an empty array", async () => {
    const res = await validateGroupTargets([], builtinOnly, noAccounts)
    expect(res.ok).toBe(false)
  })

  it("rejects a non-array", async () => {
    const res = await validateGroupTargets("claude-code/claude-opus-5", builtinOnly, noAccounts)
    expect(res.ok).toBe(false)
  })

  it("rejects more than the max target count", async () => {
    const targets = Array.from({ length: MAX_TARGETS_PER_MODEL + 1 }, (_, i) => `claude-code/model-${i}`)
    const res = await validateGroupTargets(targets, builtinOnly, noAccounts)
    expect(res.ok).toBe(false)
  })

  it("rejects a target with an unknown/unresolvable provider prefix", async () => {
    const res = await validateGroupTargets(["not-a-real-provider/model"], builtinOnly, noAccounts)
    expect(res.ok).toBe(false)
  })

  it("rejects a bare-name target (no '/') — a group can never target another group", async () => {
    const res = await validateGroupTargets(["some-other-group"], builtinOnly, noAccounts)
    expect(res.ok).toBe(false)
  })

  it("rejects duplicate targets within one group", async () => {
    const res = await validateGroupTargets(
      ["claude-code/claude-opus-5", "claude-code/claude-opus-5"],
      builtinOnly,
      noAccounts,
    )
    expect(res.ok).toBe(false)
  })

  it("rejects a non-string entry", async () => {
    const res = await validateGroupTargets([42], builtinOnly, noAccounts)
    expect(res.ok).toBe(false)
  })

  it("rejects an object entry with a non-string model", async () => {
    const res = await validateGroupTargets([{ model: 42 }], builtinOnly, noAccounts)
    expect(res.ok).toBe(false)
  })

  it("accepts a custom provider slug prefix when the resolver allows it", async () => {
    const resolvePrefix = (prefix: string) => prefix === "my-endpoint"
    const res = await validateGroupTargets(["my-endpoint/gpt-4o"], resolvePrefix, noAccounts)
    expect(res).toEqual({ ok: true, targets: [{ model: "my-endpoint/gpt-4o", account_id: null }] })
  })

  it("trims each target string", async () => {
    const res = await validateGroupTargets([" claude-code/claude-opus-5 "], builtinOnly, noAccounts)
    expect(res.ok).toBe(true)
    if (res.ok) expect(res.targets).toEqual([{ model: "claude-code/claude-opus-5", account_id: null }])
  })

  describe("account pinning (docs/providers.md § Model groups \"Account pinning\")", () => {
    it("accepts a pinned target when the account resolver approves it", async () => {
      const res = await validateGroupTargets(
        [{ model: "claude-code/claude-opus-5", account_id: "acc_1" }],
        builtinOnly,
        allAccounts,
      )
      expect(res).toEqual({
        ok: true,
        targets: [{ model: "claude-code/claude-opus-5", account_id: "acc_1" }],
      })
    })

    it("rejects a pinned target when the account resolver rejects it (foreign account, or provider mismatch)", async () => {
      const res = await validateGroupTargets(
        [{ model: "claude-code/claude-opus-5", account_id: "acc_1" }],
        builtinOnly,
        noAccounts,
      )
      expect(res.ok).toBe(false)
    })

    it("calls the account resolver with the target's own prefix, not some other target's", async () => {
      const seen: Array<[string, string]> = []
      const resolveAccount = async (accountId: string, provider: string) => {
        seen.push([accountId, provider])
        return true
      }
      await validateGroupTargets(
        [
          { model: "claude-code/claude-opus-5", account_id: "acc_cc" },
          { model: "grok/grok-4.5", account_id: "acc_grok" },
        ],
        builtinOnly,
        resolveAccount,
      )
      expect(seen).toEqual([
        ["acc_cc", "claude-code"],
        ["acc_grok", "grok"],
      ])
    })

    it("rejects a non-string account_id", async () => {
      const res = await validateGroupTargets(
        [{ model: "claude-code/claude-opus-5", account_id: 42 }],
        builtinOnly,
        allAccounts,
      )
      expect(res.ok).toBe(false)
    })

    it("rejects an empty-string account_id", async () => {
      const res = await validateGroupTargets(
        [{ model: "claude-code/claude-opus-5", account_id: "" }],
        builtinOnly,
        allAccounts,
      )
      expect(res.ok).toBe(false)
    })

    it("null account_id is the same as omitted (unpinned)", async () => {
      const res = await validateGroupTargets(
        [{ model: "claude-code/claude-opus-5", account_id: null }],
        builtinOnly,
        allAccounts,
      )
      expect(res).toEqual({
        ok: true,
        targets: [{ model: "claude-code/claude-opus-5", account_id: null }],
      })
    })

    it("the same model pinned to two different accounts is two legitimate targets (identity = model + account_id)", async () => {
      const res = await validateGroupTargets(
        [
          { model: "claude-code/claude-opus-5", account_id: "acc_1" },
          { model: "claude-code/claude-opus-5", account_id: "acc_2" },
        ],
        builtinOnly,
        allAccounts,
      )
      expect(res.ok).toBe(true)
      if (res.ok) expect(res.targets).toHaveLength(2)
    })

    it("the same model once pinned and once unpinned is two legitimate targets", async () => {
      const res = await validateGroupTargets(
        [
          { model: "claude-code/claude-opus-5", account_id: "acc_1" },
          { model: "claude-code/claude-opus-5" },
        ],
        builtinOnly,
        allAccounts,
      )
      expect(res.ok).toBe(true)
      if (res.ok) expect(res.targets).toHaveLength(2)
    })

    it("rejects a duplicate (model, account_id) pair — same model pinned to the same account twice", async () => {
      const res = await validateGroupTargets(
        [
          { model: "claude-code/claude-opus-5", account_id: "acc_1" },
          { model: "claude-code/claude-opus-5", account_id: "acc_1" },
        ],
        builtinOnly,
        allAccounts,
      )
      expect(res.ok).toBe(false)
    })

    it("rejects two unpinned targets with the same model (both normalize to account_id: null)", async () => {
      const res = await validateGroupTargets(
        [{ model: "claude-code/claude-opus-5" }, "claude-code/claude-opus-5"],
        builtinOnly,
        allAccounts,
      )
      expect(res.ok).toBe(false)
    })
  })
})

describe("parseGroupTargets — tolerant parse (docs/database.md model_groups.targets_json)", () => {
  it("returns [] for null", () => {
    expect(parseGroupTargets(null)).toEqual([])
  })

  it("parses a stored JSON array of plain strings (v3.0.0 rows) as unpinned targets", () => {
    expect(parseGroupTargets('["claude-code/claude-opus-5","grok/grok-4.5"]')).toEqual([
      { model: "claude-code/claude-opus-5", account_id: null },
      { model: "grok/grok-4.5", account_id: null },
    ])
  })

  it("returns [] for malformed JSON", () => {
    expect(parseGroupTargets("not json")).toEqual([])
  })

  it("returns [] for a JSON value that isn't an array", () => {
    expect(parseGroupTargets('{"a":1}')).toEqual([])
  })

  it("parses an object entry with account_id as a pinned target", () => {
    expect(parseGroupTargets('[{"model":"claude-code/claude-opus-5","account_id":"acc_1"}]')).toEqual([
      { model: "claude-code/claude-opus-5", account_id: "acc_1" },
    ])
  })

  it("an object entry with no account_id normalizes to account_id: null", () => {
    expect(parseGroupTargets('[{"model":"claude-code/claude-opus-5"}]')).toEqual([
      { model: "claude-code/claude-opus-5", account_id: null },
    ])
  })

  it("tolerates a future per-target field alongside model/account_id", () => {
    expect(
      parseGroupTargets('[{"model":"claude-code/claude-opus-5","account_id":"acc_1","weight":2}]'),
    ).toEqual([{ model: "claude-code/claude-opus-5", account_id: "acc_1" }])
  })

  it("a non-string account_id normalizes to null rather than propagating garbage", () => {
    expect(parseGroupTargets('[{"model":"claude-code/claude-opus-5","account_id":42}]')).toEqual([
      { model: "claude-code/claude-opus-5", account_id: null },
    ])
  })

  it("mixes string-shorthand and object entries in one array", () => {
    expect(
      parseGroupTargets('["grok/grok-4.5",{"model":"codex/gpt-5.2","account_id":"acc_1"}]'),
    ).toEqual([
      { model: "grok/grok-4.5", account_id: null },
      { model: "codex/gpt-5.2", account_id: "acc_1" },
    ])
  })

  it("drops an entry that is neither a string nor an object with a string 'model' field", () => {
    expect(parseGroupTargets('[42, {"notModel":"x"}, "grok/grok-4.5"]')).toEqual([
      { model: "grok/grok-4.5", account_id: null },
    ])
  })
})

describe("limits", () => {
  it("MAX_MODEL_GROUPS_PER_USER is 50 per docs/providers.md", () => {
    expect(MAX_MODEL_GROUPS_PER_USER).toBe(50)
  })

  it("MAX_TARGETS_PER_MODEL is 20 per docs/providers.md", () => {
    expect(MAX_TARGETS_PER_MODEL).toBe(20)
  })

  it("MAX_MODELS_PER_GROUP is 20 per docs/providers.md", () => {
    expect(MAX_MODELS_PER_GROUP).toBe(20)
  })
})
