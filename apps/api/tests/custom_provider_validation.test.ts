import { describe, expect, it } from "vitest"
import {
  MAX_CUSTOM_PROVIDERS_PER_USER,
  RESERVED_SLUGS,
  isCustomProviderFormat,
  isModelsMode,
  maskApiKey,
  parseManualModels,
  validateApiKey,
  validateBaseUrlLength,
  validateManualModels,
  validateName,
  validateSlug,
} from "../src/utils/custom_provider"

describe("validateSlug", () => {
  it("accepts a plain lowercase slug", () => {
    expect(validateSlug("my-endpoint")).toBeNull()
  })

  it("accepts the minimum length (2 chars)", () => {
    expect(validateSlug("ab")).toBeNull()
  })

  it("accepts the maximum length (32 chars)", () => {
    expect(validateSlug("a".repeat(32))).toBeNull()
  })

  it("rejects below minimum length", () => {
    expect(validateSlug("a")).not.toBeNull()
    expect(validateSlug("")).not.toBeNull()
  })

  it("rejects above maximum length", () => {
    expect(validateSlug("a".repeat(33))).not.toBeNull()
  })

  it("rejects uppercase", () => {
    expect(validateSlug("My-Endpoint")).not.toBeNull()
  })

  it("rejects a leading hyphen", () => {
    expect(validateSlug("-abc")).not.toBeNull()
  })

  it("rejects a trailing hyphen", () => {
    expect(validateSlug("abc-")).not.toBeNull()
  })

  it("rejects whitespace and other punctuation", () => {
    expect(validateSlug("ab c")).not.toBeNull()
    expect(validateSlug("ab_c")).not.toBeNull()
    expect(validateSlug("ab.c")).not.toBeNull()
    expect(validateSlug("ab/c")).not.toBeNull()
  })

  it("accepts internal hyphens and digits", () => {
    expect(validateSlug("my-2nd-endpoint9")).toBeNull()
  })

  it("rejects every reserved slug", () => {
    for (const slug of RESERVED_SLUGS) {
      expect(validateSlug(slug), slug).not.toBeNull()
    }
  })

  it("reserved list covers builtins and route-shaped segments", () => {
    for (const s of ["claude-code", "codex", "grok", "api", "admin", "models", "kano-proxy"]) {
      expect(RESERVED_SLUGS.has(s)).toBe(true)
    }
  })
})

describe("validateName", () => {
  it("accepts 1-64 chars", () => {
    expect(validateName("a")).toBeNull()
    expect(validateName("a".repeat(64))).toBeNull()
  })
  it("rejects empty", () => {
    expect(validateName("")).not.toBeNull()
  })
  it("rejects over 64 chars", () => {
    expect(validateName("a".repeat(65))).not.toBeNull()
  })
})

describe("validateApiKey", () => {
  it("accepts 1-512 chars", () => {
    expect(validateApiKey("k")).toBeNull()
    expect(validateApiKey("k".repeat(512))).toBeNull()
  })
  it("rejects empty", () => {
    expect(validateApiKey("")).not.toBeNull()
  })
  it("rejects over 512 chars", () => {
    expect(validateApiKey("k".repeat(513))).not.toBeNull()
  })
})

describe("validateBaseUrlLength", () => {
  it("accepts up to 300 chars", () => {
    expect(validateBaseUrlLength("https://example.com/" + "a".repeat(279))).toBeNull()
  })
  it("rejects over 300 chars", () => {
    expect(validateBaseUrlLength("https://example.com/" + "a".repeat(400))).not.toBeNull()
  })
})

describe("validateManualModels", () => {
  it("treats an omitted field as an empty, valid list", () => {
    expect(validateManualModels(undefined)).toEqual({ ok: true, models: [] })
  })

  it("accepts a list of trimmed model ids", () => {
    expect(validateManualModels([" model-a ", "model-b"])).toEqual({
      ok: true,
      models: ["model-a", "model-b"],
    })
  })

  it("allows '/' in an entry (namespaced upstream ids)", () => {
    expect(validateManualModels(["org/model"])).toEqual({ ok: true, models: ["org/model"] })
  })

  it("rejects a non-array", () => {
    expect(validateManualModels("model-a").ok).toBe(false)
  })

  it("rejects more than 100 entries", () => {
    const many = Array.from({ length: 101 }, (_, i) => `m${i}`)
    expect(validateManualModels(many).ok).toBe(false)
  })

  it("accepts exactly 100 entries", () => {
    const many = Array.from({ length: 100 }, (_, i) => `m${i}`)
    expect(validateManualModels(many).ok).toBe(true)
  })

  it("rejects a non-string entry", () => {
    expect(validateManualModels([123]).ok).toBe(false)
  })

  it("rejects an empty-after-trim entry", () => {
    expect(validateManualModels(["   "]).ok).toBe(false)
  })

  it("rejects an entry over 128 characters", () => {
    expect(validateManualModels(["a".repeat(129)]).ok).toBe(false)
  })

  it("accepts an entry at exactly 128 characters", () => {
    expect(validateManualModels(["a".repeat(128)]).ok).toBe(true)
  })

  it("rejects whitespace inside an entry", () => {
    expect(validateManualModels(["model a"]).ok).toBe(false)
  })
})

describe("parseManualModels", () => {
  it("returns [] for null", () => {
    expect(parseManualModels(null)).toEqual([])
  })
  it("parses a stored JSON array", () => {
    expect(parseManualModels('["a","b"]')).toEqual(["a", "b"])
  })
  it("returns [] for malformed JSON", () => {
    expect(parseManualModels("not json")).toEqual([])
  })
  it("filters out non-string entries", () => {
    expect(parseManualModels('["a", 1, "b"]')).toEqual(["a", "b"])
  })
})

describe("maskApiKey", () => {
  it("masks a long key as first 6 + … + last 4", () => {
    expect(maskApiKey("sk-abcdefghijf3a2")).toBe("sk-abc…f3a2")
  })

  it("masks a key at exactly the 12-char boundary using the long form", () => {
    expect(maskApiKey("123456789012")).toBe("123456…9012")
  })

  it("masks everything but the last 2 chars for a short key", () => {
    expect(maskApiKey("shortkey")).toBe("******ey")
  })

  it("handles a very short key without throwing", () => {
    expect(maskApiKey("ab")).toBe("ab")
    expect(maskApiKey("a")).toBe("a")
  })

  it("never contains the full key body for a long key", () => {
    const key = "sk-verysecretlongapikeyvalue1234"
    const masked = maskApiKey(key)
    expect(masked).not.toContain(key.slice(6, -4))
  })
})

describe("isCustomProviderFormat / isModelsMode", () => {
  it("accepts exactly openai/anthropic", () => {
    expect(isCustomProviderFormat("openai")).toBe(true)
    expect(isCustomProviderFormat("anthropic")).toBe(true)
    expect(isCustomProviderFormat("openrouter")).toBe(false)
    expect(isCustomProviderFormat(undefined)).toBe(false)
  })

  it("accepts exactly auto/manual", () => {
    expect(isModelsMode("auto")).toBe(true)
    expect(isModelsMode("manual")).toBe(true)
    expect(isModelsMode("live")).toBe(false)
  })
})

describe("MAX_CUSTOM_PROVIDERS_PER_USER", () => {
  it("is 20", () => {
    expect(MAX_CUSTOM_PROVIDERS_PER_USER).toBe(20)
  })
})
