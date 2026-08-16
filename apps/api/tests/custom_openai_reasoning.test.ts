import { describe, expect, it } from "vitest"
import {
  parseTabbyQwenTemplateError,
  parseUnsupportedEffortRejection,
  remapUnsupportedEffortBody,
} from "../src/providers/custom_openai_reasoning"

const TABBY_DETAIL =
  "TemplateError: Unexpected reasoning effort high. Supported types are xhigh (default), medium, and low."

const TABBY_FASTAPI = JSON.stringify({ detail: TABBY_DETAIL })

describe("parseTabbyQwenTemplateError", () => {
  it("parses the FastAPI Tabby payload in document order", () => {
    expect(parseTabbyQwenTemplateError(TABBY_FASTAPI)).toEqual({
      rejected: "high",
      allowed: ["xhigh", "medium", "low"],
    })
  })

  it("matches a bare TemplateError string", () => {
    expect(parseTabbyQwenTemplateError(TABBY_DETAIL)).toEqual({
      rejected: "high",
      allowed: ["xhigh", "medium", "low"],
    })
  })

  it("strips (default) markers from the supported list", () => {
    expect(
      parseTabbyQwenTemplateError(
        "Unexpected reasoning effort medium. Supported types are low (default) and high.",
      ),
    ).toEqual({
      rejected: "medium",
      allowed: ["low", "high"],
    })
  })

  it("returns null for unrecognized 400 text", () => {
    expect(parseTabbyQwenTemplateError('{"error":"model not found"}')).toBeNull()
    expect(parseTabbyQwenTemplateError("bad request")).toBeNull()
  })

  it("returns null for a garbage rejected token or empty supported list", () => {
    expect(
      parseTabbyQwenTemplateError(
        "TemplateError: Unexpected reasoning effort ultra. Supported types are xhigh, medium, and low.",
      ),
    ).toBeNull()
    expect(
      parseTabbyQwenTemplateError(
        "TemplateError: Unexpected reasoning effort high. Supported types are .",
      ),
    ).toBeNull()
  })
})

describe("parseUnsupportedEffortRejection", () => {
  it("returns the first registered parser's result", () => {
    expect(parseUnsupportedEffortRejection(TABBY_FASTAPI)).toEqual({
      rejected: "high",
      allowed: ["xhigh", "medium", "low"],
    })
  })

  it("returns null when no parser matches", () => {
    expect(parseUnsupportedEffortRejection("Internal Server Error")).toBeNull()
  })
})

describe("remapUnsupportedEffortBody", () => {
  it("rewrites only reasoning_effort to the nearest allowed token", () => {
    expect(
      remapUnsupportedEffortBody(
        { model: "qwen", messages: [], reasoning_effort: "high", temperature: 0.7 },
        TABBY_FASTAPI,
      ),
    ).toEqual({
      model: "qwen",
      messages: [],
      reasoning_effort: "xhigh",
      temperature: 0.7,
    })
  })

  it("does not remap when no reasoning_effort was sent", () => {
    expect(remapUnsupportedEffortBody({ model: "qwen" }, TABBY_FASTAPI)).toBeNull()
  })

  it("does not remap an unrecognized 400", () => {
    expect(
      remapUnsupportedEffortBody(
        { reasoning_effort: "high" },
        '{"error":"context length exceeded"}',
      ),
    ).toBeNull()
  })

  it("does not remap when the rejected token is already allowed", () => {
    expect(
      remapUnsupportedEffortBody(
        { reasoning_effort: "medium" },
        "Unexpected reasoning effort medium. Supported types are medium and high.",
      ),
    ).toBeNull()
  })
})
