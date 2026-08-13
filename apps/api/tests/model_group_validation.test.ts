import { describe, expect, it } from "vitest"
import {
  MAX_ALIAS_LENGTH,
  MAX_ALIASES_PER_GROUP,
  MAX_DISPLAY_NAME_LENGTH,
  MAX_MODEL_GROUPS_PER_USER,
  MAX_TARGETS_PER_GROUP,
  validateAlias,
  validateAliases,
  validateDisplayName,
  validateGroupTargets,
} from "../src/utils/model_group"
import { parseGroupTargets } from "../src/db/model_groups"

describe("validateAlias", () => {
  it("accepts a plain bare alias", () => {
    expect(validateAlias("opus")).toBeNull()
  })

  it("accepts the maximum length (128 chars)", () => {
    expect(validateAlias("a".repeat(MAX_ALIAS_LENGTH))).toBeNull()
  })

  it("rejects empty", () => {
    expect(validateAlias("")).not.toBeNull()
  })

  it("rejects above maximum length", () => {
    expect(validateAlias("a".repeat(MAX_ALIAS_LENGTH + 1))).not.toBeNull()
  })

  it("rejects internal whitespace", () => {
    expect(validateAlias("my alias")).not.toBeNull()
  })

  it("rejects a leading/trailing space (caller is expected to trim first, but this still must reject)", () => {
    expect(validateAlias(" opus")).not.toBeNull()
    expect(validateAlias("opus ")).not.toBeNull()
  })

  it("rejects any '/'", () => {
    expect(validateAlias("claude-code/opus")).not.toBeNull()
    expect(validateAlias("a/b")).not.toBeNull()
  })

  it("accepts punctuation other than whitespace and '/'", () => {
    expect(validateAlias("gpt-4o")).toBeNull()
    expect(validateAlias("my_group.v2")).toBeNull()
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

describe("validateAliases", () => {
  it("accepts a single alias", () => {
    expect(validateAliases(["gpt-4o"])).toEqual({ ok: true, aliases: ["gpt-4o"] })
  })

  it("accepts up to the max alias count (10)", () => {
    const aliases = Array.from({ length: MAX_ALIASES_PER_GROUP }, (_, i) => `alias-${i}`)
    const res = validateAliases(aliases)
    expect(res.ok).toBe(true)
  })

  it("rejects an empty array", () => {
    expect(validateAliases([]).ok).toBe(false)
  })

  it("rejects a non-array", () => {
    expect(validateAliases("gpt-4o").ok).toBe(false)
  })

  it("rejects more than the max alias count", () => {
    const aliases = Array.from({ length: MAX_ALIASES_PER_GROUP + 1 }, (_, i) => `alias-${i}`)
    expect(validateAliases(aliases).ok).toBe(false)
  })

  it("rejects a non-string entry", () => {
    expect(validateAliases([42]).ok).toBe(false)
  })

  it("rejects an entry that fails validateAlias (e.g. contains whitespace)", () => {
    expect(validateAliases(["bad alias"]).ok).toBe(false)
  })

  it("trims each alias", () => {
    const res = validateAliases([" gpt-4o "])
    expect(res).toEqual({ ok: true, aliases: ["gpt-4o"] })
  })

  it("rejects an in-payload duplicate (exact, case-sensitive)", () => {
    expect(validateAliases(["gpt-4o", "gpt-4o"]).ok).toBe(false)
  })

  it("a duplicate after trimming is still caught", () => {
    expect(validateAliases(["gpt-4o", " gpt-4o "]).ok).toBe(false)
  })

  it("does not treat differently-cased aliases as duplicates (matched exactly, case-sensitive)", () => {
    const res = validateAliases(["GPT-4o", "gpt-4o"])
    expect(res).toEqual({ ok: true, aliases: ["GPT-4o", "gpt-4o"] })
  })

  it("accepts multiple distinct aliases in order", () => {
    const res = validateAliases(["gpt-4o", "gpt-4", "gpt-4-turbo"])
    expect(res).toEqual({ ok: true, aliases: ["gpt-4o", "gpt-4", "gpt-4-turbo"] })
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
    const targets = Array.from({ length: MAX_TARGETS_PER_GROUP }, (_, i) => `claude-code/model-${i}`)
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
    const targets = Array.from({ length: MAX_TARGETS_PER_GROUP + 1 }, (_, i) => `claude-code/model-${i}`)
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

  it("MAX_TARGETS_PER_GROUP is 20 per docs/providers.md", () => {
    expect(MAX_TARGETS_PER_GROUP).toBe(20)
  })

  it("MAX_ALIASES_PER_GROUP is 10 per docs/providers.md", () => {
    expect(MAX_ALIASES_PER_GROUP).toBe(10)
  })
})
