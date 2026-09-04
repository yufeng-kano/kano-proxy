/**
 * Responses API ↔ Chat Completions conversion (docs/api.md § `POST
 * /openai/v1/responses`): the request mapper, the streaming and non-stream
 * output converters, the native-path error-frame rewrite and SSE collector,
 * and the codex native body builder. Pure functions, no network.
 */
import { describe, expect, it } from "vitest"
import {
  UnsupportedResponsesField,
  WEB_SEARCH_STUB_TOOL,
  collectResponsesSse,
  openaiSseToResponsesStream,
  openaiToResponsesObject,
  responsesToChatRequest,
  rewriteOpenAIErrorFramesToResponses,
} from "../src/proxy/responses_openai"
import { buildCodexNativeRequestBody } from "../src/providers/codex"
import { createOpenAISseUsageSniffer, fromOpenAIUsage } from "../src/logging/usage_capture"

function sseBody(lines: string[]): ReadableStream<Uint8Array> {
  const text = lines.map((l) => `${l}\n\n`).join("")
  const bytes = new TextEncoder().encode(text)
  return new ReadableStream({
    start(controller) {
      // Split mid-line to prove the converters carry partial lines correctly.
      for (let i = 0; i < bytes.length; i += 7) controller.enqueue(bytes.subarray(i, i + 7))
      controller.close()
    },
  })
}

async function drain(stream: ReadableStream<Uint8Array>): Promise<string> {
  const reader = stream.getReader()
  let out = ""
  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    out += new TextDecoder().decode(value, { stream: true })
  }
  return out
}

/** Parse a Responses SSE text into its event payloads (in order). */
function events(text: string): Array<Record<string, unknown> & { type: string }> {
  return text
    .split("\n")
    .filter((l) => l.startsWith("data:"))
    .map((l) => JSON.parse(l.slice(5).trim()))
}

const chatChunk = (delta: Record<string, unknown>, extra: Record<string, unknown> = {}) =>
  `data: ${JSON.stringify({
    id: "chatcmpl_x",
    object: "chat.completion.chunk",
    choices: [{ index: 0, delta, finish_reason: null }],
    ...extra,
  })}`

/** The shape the Codex CLI 0.150.1 actually sent (captured 2026-09-04). */
function codexCliRequest(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    model: "claude-code/claude-opus-5",
    instructions: "You are Codex.",
    input: [
      {
        type: "message",
        role: "developer",
        content: [
          { type: "input_text", text: "<environment_context>cwd=/tmp</environment_context>" },
          { type: "input_text", text: "AGENTS.md says hi" },
        ],
      },
      { type: "message", role: "user", content: [{ type: "input_text", text: "say hi" }] },
    ],
    tools: [
      {
        type: "function",
        name: "exec_command",
        description: "Run a command",
        strict: false,
        parameters: { type: "object", properties: { cmd: { type: "string" } }, required: ["cmd"] },
      },
      {
        type: "namespace",
        name: "multi_agent_v1",
        description: "Tools in the multi_agent_v1 namespace.",
        tools: [
          {
            type: "function",
            name: "spawn_agent",
            description: "Spawn a sub-agent",
            strict: false,
            parameters: { type: "object", properties: { message: { type: "string" } } },
          },
        ],
      },
      { type: "web_search", external_web_access: false },
    ],
    tool_choice: "auto",
    parallel_tool_calls: true,
    reasoning: { summary: "auto" },
    store: false,
    stream: true,
    include: ["reasoning.encrypted_content"],
    prompt_cache_key: "01a06ce4-68a8-7892-bcf5-b64013c7f279",
    client_metadata: { session_id: "01a06ce4-68a8-7892-bcf5-b64013c7f279" },
    ...overrides,
  }
}

describe("responsesToChatRequest", () => {
  it("converts the Codex CLI request: instructions + developer → system, namespace flattened, web_search stubbed", () => {
    const { chat, toolNames } = responsesToChatRequest(codexCliRequest())
    expect(chat.model).toBe("claude-code/claude-opus-5")
    expect(chat.stream).toBe(true)
    expect(chat.messages).toEqual([
      { role: "system", content: "You are Codex." },
      { role: "system", content: "<environment_context>cwd=/tmp</environment_context>\n\nAGENTS.md says hi" },
      { role: "user", content: "say hi" },
    ])
    const tools = chat.tools as Array<{ type: string; function: { name: string; description?: string } }>
    expect(tools.map((t) => t.function.name)).toEqual([
      "exec_command",
      "multi_agent_v1__spawn_agent",
      "web_search",
    ])
    expect(tools[2]).toEqual(WEB_SEARCH_STUB_TOOL)
    expect(toolNames.get("multi_agent_v1__spawn_agent")).toEqual({
      kind: "function",
      namespace: "multi_agent_v1",
      name: "spawn_agent",
    })
    expect(chat.tool_choice).toBe("auto")
    expect(chat.parallel_tool_calls).toBe(true)
    expect(chat.prompt_cache_key).toBe("01a06ce4-68a8-7892-bcf5-b64013c7f279")
    // Nothing the Chat wire has no field for leaks into the passthrough body.
    expect(chat).not.toHaveProperty("client_metadata")
    expect(chat).not.toHaveProperty("include")
    expect(chat).not.toHaveProperty("store")
    // No effort was sent, so none is invented.
    expect(chat).not.toHaveProperty("reasoning_effort")
  })

  it("does not add the web_search stub when the client already defines a web_search function", () => {
    const { chat } = responsesToChatRequest({
      model: "grok/grok-4.5",
      input: "hi",
      tools: [
        { type: "function", name: "web_search", parameters: { type: "object" } },
        { type: "web_search" },
      ],
    })
    const tools = chat.tools as Array<{ function: { name: string } }>
    expect(tools).toHaveLength(1)
    expect(tools[0]!.function.name).toBe("web_search")
    expect(tools[0]).not.toEqual(WEB_SEARCH_STUB_TOOL)
  })

  it("folds a multi-turn tool round into Chat assistant/tool messages, dropping reasoning items", () => {
    const { chat } = responsesToChatRequest({
      model: "claude-code/claude-opus-5",
      input: [
        { type: "message", role: "user", content: "list files" },
        { type: "reasoning", id: "rs_1", summary: [], encrypted_content: "gAAAA…" },
        { type: "message", role: "assistant", content: [{ type: "output_text", text: "Sure." }] },
        {
          type: "function_call",
          id: "fc_1",
          call_id: "call_1",
          name: "exec_command",
          arguments: '{"cmd":"ls"}',
        },
        {
          type: "function_call",
          id: "fc_2",
          call_id: "call_2",
          name: "spawn_agent",
          namespace: "multi_agent_v1",
          arguments: '{"message":"go"}',
        },
        { type: "function_call_output", call_id: "call_1", output: "a.txt\nb.txt" },
        {
          type: "function_call_output",
          call_id: "call_2",
          output: [{ type: "input_text", text: "spawned" }],
        },
        { type: "custom_tool_call", call_id: "call_3", name: "apply_patch", input: "*** Begin Patch" },
        { type: "custom_tool_call_output", call_id: "call_3", output: "ok" },
        { role: "user", content: [{ type: "input_text", text: "thanks" }] },
      ],
    })
    expect(chat.messages).toEqual([
      { role: "user", content: "list files" },
      {
        role: "assistant",
        content: "Sure.",
        tool_calls: [
          { id: "call_1", type: "function", function: { name: "exec_command", arguments: '{"cmd":"ls"}' } },
          {
            id: "call_2",
            type: "function",
            function: { name: "multi_agent_v1__spawn_agent", arguments: '{"message":"go"}' },
          },
        ],
      },
      { role: "tool", tool_call_id: "call_1", content: "a.txt\nb.txt" },
      { role: "tool", tool_call_id: "call_2", content: "spawned" },
      {
        role: "assistant",
        content: null,
        tool_calls: [
          {
            id: "call_3",
            type: "function",
            function: { name: "apply_patch", arguments: '{"input":"*** Begin Patch"}' },
          },
        ],
      },
      { role: "tool", tool_call_id: "call_3", content: "ok" },
      { role: "user", content: "thanks" },
    ])
  })

  it("maps images, custom tools, text.format, reasoning.effort, max_output_tokens and tool_choice", () => {
    const { chat, toolNames } = responsesToChatRequest({
      model: "grok/grok-4.5",
      input: [
        {
          type: "message",
          role: "user",
          content: [
            { type: "input_text", text: "what is this" },
            { type: "input_image", image_url: "data:image/png;base64,AAAA", detail: "high" },
          ],
        },
      ],
      tools: [{ type: "custom", name: "apply_patch", description: "Patch files" }],
      tool_choice: { type: "function", name: "apply_patch" },
      text: { format: { type: "json_schema", name: "out", schema: { type: "object" }, strict: true }, verbosity: "low" },
      reasoning: { effort: "high", summary: "detailed" },
      max_output_tokens: 123,
      temperature: 0.2,
      top_p: 0.9,
    })
    expect(chat.messages).toEqual([
      {
        role: "user",
        content: [
          { type: "text", text: "what is this" },
          { type: "image_url", image_url: { url: "data:image/png;base64,AAAA", detail: "high" } },
        ],
      },
    ])
    const tools = chat.tools as Array<{ function: { name: string; parameters: { required: string[] } } }>
    expect(tools[0]!.function.name).toBe("apply_patch")
    expect(tools[0]!.function.parameters.required).toEqual(["input"])
    expect(toolNames.get("apply_patch")).toEqual({ kind: "custom", name: "apply_patch" })
    expect(chat.tool_choice).toEqual({ type: "function", function: { name: "apply_patch" } })
    expect(chat.response_format).toEqual({
      type: "json_schema",
      json_schema: { name: "out", schema: { type: "object" }, strict: true },
    })
    expect(chat.reasoning_effort).toBe("high")
    expect(chat.max_tokens).toBe(123)
    expect(chat.temperature).toBe(0.2)
    expect(chat.top_p).toBe(0.9)
  })

  it("drops tool_choice when every tool was a hosted one that got dropped", () => {
    const { chat } = responsesToChatRequest({
      model: "grok/grok-4.5",
      input: "hi",
      tools: [{ type: "image_generation" }],
      tool_choice: "required",
    })
    expect(chat).not.toHaveProperty("tools")
    expect(chat).not.toHaveProperty("tool_choice")
  })

  it.each([
    [{ previous_response_id: "resp_1" }, "previous_response_id"],
    [{ conversation: "conv_1" }, "conversation"],
    [{ background: true }, "background"],
    [{ input: [{ type: "item_reference", id: "msg_1" }] }, "input.item_reference"],
    [
      { input: [{ type: "message", role: "user", content: [{ type: "input_file", file_id: "f" }] }] },
      "input.content.input_file",
    ],
  ])("rejects %j as unsupported_field", (extra, field) => {
    let caught: unknown
    try {
      responsesToChatRequest({ model: "grok/grok-4.5", input: "hi", ...extra })
    } catch (e) {
      caught = e
    }
    expect(caught).toBeInstanceOf(UnsupportedResponsesField)
    expect((caught as UnsupportedResponsesField).field).toBe(field)
  })
})

describe("openaiSseToResponsesStream", () => {
  const toolNames = new Map([
    ["exec_command", { kind: "function" as const, name: "exec_command" }],
    ["multi_agent_v1__spawn_agent", { kind: "function" as const, namespace: "multi_agent_v1", name: "spawn_agent" }],
    ["apply_patch", { kind: "custom" as const, name: "apply_patch" }],
  ])

  it("emits the Responses event sequence for reasoning, text, and a function call, with usage", async () => {
    const text = await drain(
      openaiSseToResponsesStream(
        sseBody([
          chatChunk({ role: "assistant", content: "" }),
          chatChunk({ reasoning_content: "thinking " }),
          chatChunk({ reasoning_content: "hard" }),
          chatChunk({ content: "Hel" }),
          chatChunk({ content: "lo" }),
          chatChunk({
            tool_calls: [{ index: 0, id: "call_1", type: "function", function: { name: "exec_command", arguments: "" } }],
          }),
          chatChunk({ tool_calls: [{ index: 0, function: { arguments: '{"cmd":' } }] }),
          chatChunk({ tool_calls: [{ index: 0, function: { arguments: '"ls"}' } }] }),
          `data: ${JSON.stringify({
            id: "chatcmpl_x",
            object: "chat.completion.chunk",
            choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }],
            usage: {
              prompt_tokens: 100,
              completion_tokens: 20,
              prompt_tokens_details: { cached_tokens: 60 },
              completion_tokens_details: { reasoning_tokens: 5 },
            },
          })}`,
          "data: [DONE]",
        ]),
        { model: "claude-code/claude-opus-5", toolNames },
      ),
    )
    // event: lines accompany every data: line.
    expect(text.startsWith("event: response.created\ndata: ")).toBe(true)
    const evs = events(text)
    expect(evs.map((e) => e.type)).toEqual([
      "response.created",
      "response.in_progress",
      "response.output_item.added",
      "response.reasoning_summary_part.added",
      "response.reasoning_summary_text.delta",
      "response.reasoning_summary_text.delta",
      "response.reasoning_summary_text.done",
      "response.reasoning_summary_part.done",
      "response.output_item.done",
      "response.output_item.added",
      "response.content_part.added",
      "response.output_text.delta",
      "response.output_text.delta",
      "response.output_text.done",
      "response.content_part.done",
      "response.output_item.done",
      "response.output_item.added",
      "response.function_call_arguments.delta",
      "response.function_call_arguments.delta",
      "response.function_call_arguments.done",
      "response.output_item.done",
      "response.completed",
    ])
    // Monotonic sequence numbers.
    expect(evs.map((e) => e.sequence_number)).toEqual(evs.map((_, i) => i))

    const completed = evs.at(-1)!.response as Record<string, unknown>
    expect(completed.status).toBe("completed")
    expect(completed.model).toBe("claude-code/claude-opus-5")
    const output = completed.output as Array<Record<string, unknown>>
    expect(output).toHaveLength(3)
    expect(output[0]).toMatchObject({ type: "reasoning", summary: [{ type: "summary_text", text: "thinking hard" }] })
    expect(output[1]).toMatchObject({
      type: "message",
      role: "assistant",
      status: "completed",
      content: [{ type: "output_text", text: "Hello", annotations: [] }],
    })
    expect(output[2]).toMatchObject({
      type: "function_call",
      call_id: "call_1",
      name: "exec_command",
      arguments: '{"cmd":"ls"}',
      status: "completed",
    })
    expect(output[2]).not.toHaveProperty("namespace")
    expect(completed.usage).toEqual({
      input_tokens: 100,
      output_tokens: 20,
      total_tokens: 120,
      input_tokens_details: { cached_tokens: 60 },
      output_tokens_details: { reasoning_tokens: 5 },
    })
    expect(String(output[0]!.id).startsWith("rs_")).toBe(true)
    expect(String(output[1]!.id).startsWith("msg_")).toBe(true)
    expect(String(output[2]!.id).startsWith("fc_")).toBe(true)
  })

  it("puts namespace + bare name back on a flattened tool call, and emits a custom tool call whole", async () => {
    const text = await drain(
      openaiSseToResponsesStream(
        sseBody([
          chatChunk({
            tool_calls: [
              {
                index: 0,
                id: "call_a",
                type: "function",
                function: { name: "multi_agent_v1__spawn_agent", arguments: '{"message":"go"}' },
              },
            ],
          }),
          chatChunk({
            tool_calls: [{ index: 1, id: "call_b", type: "function", function: { name: "apply_patch", arguments: "" } }],
          }),
          chatChunk({ tool_calls: [{ index: 1, function: { arguments: '{"input":"*** Begin' } }] }),
          chatChunk({ tool_calls: [{ index: 1, function: { arguments: ' Patch"}' } }] }),
          `data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] })}`,
          "data: [DONE]",
        ]),
        { model: "m", toolNames },
      ),
    )
    const evs = events(text)
    const added = evs.filter((e) => e.type === "response.output_item.added").map((e) => e.item)
    expect(added[0]).toMatchObject({
      type: "function_call",
      call_id: "call_a",
      name: "spawn_agent",
      namespace: "multi_agent_v1",
      status: "in_progress",
    })
    // The custom tool item is announced only once its input is complete.
    expect(added[1]).toMatchObject({
      type: "custom_tool_call",
      call_id: "call_b",
      name: "apply_patch",
      input: "*** Begin Patch",
    })
    expect(evs.some((e) => e.type === "response.function_call_arguments.delta" && (e.item_id as string).startsWith("ctc_"))).toBe(false)
    const done = evs.filter((e) => e.type === "response.output_item.done").map((e) => e.item as Record<string, unknown>)
    expect(done).toHaveLength(2)
    expect(done[0]).toMatchObject({ name: "spawn_agent", namespace: "multi_agent_v1", arguments: '{"message":"go"}' })
    expect(done[1]).toMatchObject({ type: "custom_tool_call", input: "*** Begin Patch" })
  })

  it("turns an in-stream OpenAI error line into response.failed and stops", async () => {
    const text = await drain(
      openaiSseToResponsesStream(
        sseBody([
          chatChunk({ content: "partial" }),
          'data: {"error":{"message":"All upstream accounts unavailable","type":"api_error","code":"upstream_unavailable"}}',
          chatChunk({ content: "never" }),
          "data: [DONE]",
        ]),
        { model: "m", toolNames: new Map() },
      ),
    )
    const evs = events(text)
    const last = evs.at(-1)!
    expect(last.type).toBe("response.failed")
    expect((last.response as Record<string, unknown>).status).toBe("failed")
    expect((last.response as Record<string, unknown>).error).toEqual({
      code: "upstream_unavailable",
      message: "All upstream accounts unavailable",
    })
    // The message item that was open is closed before failing, and nothing follows.
    expect(evs.filter((e) => e.type === "response.output_text.delta")).toHaveLength(1)
    expect(evs.some((e) => e.type === "response.completed")).toBe(false)
  })

  it("ends a truncated turn as response.incomplete with max_output_tokens", async () => {
    const text = await drain(
      openaiSseToResponsesStream(
        sseBody([
          chatChunk({ content: "x" }),
          `data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: "length" }] })}`,
          "data: [DONE]",
        ]),
        { model: "m", toolNames: new Map() },
      ),
    )
    const last = events(text).at(-1)!
    expect(last.type).toBe("response.incomplete")
    expect((last.response as Record<string, unknown>).incomplete_details).toEqual({ reason: "max_output_tokens" })
  })

  it("still completes on a clean EOF without [DONE]", async () => {
    const text = await drain(
      openaiSseToResponsesStream(sseBody([chatChunk({ content: "x" })]), { model: "m", toolNames: new Map() }),
    )
    expect(events(text).at(-1)!.type).toBe("response.completed")
  })
})

describe("openaiToResponsesObject", () => {
  it("builds the same items from a non-stream completion", () => {
    const out = openaiToResponsesObject(
      {
        id: "chatcmpl_1",
        created: 1700000000,
        choices: [
          {
            index: 0,
            message: {
              role: "assistant",
              content: "Hi",
              reasoning_content: "r",
              tool_calls: [
                { id: "call_1", type: "function", function: { name: "ns__t", arguments: "{}" } },
                { id: "call_2", type: "function", function: { name: "apply_patch", arguments: '{"input":"p"}' } },
              ],
            },
            finish_reason: "tool_calls",
          },
        ],
        usage: { prompt_tokens: 3, completion_tokens: 4 },
      },
      {
        model: "raw/model",
        toolNames: new Map([
          ["ns__t", { kind: "function", namespace: "ns", name: "t" }],
          ["apply_patch", { kind: "custom", name: "apply_patch" }],
        ]),
      },
    )
    expect(out.object).toBe("response")
    expect(out.status).toBe("completed")
    expect(out.model).toBe("raw/model")
    expect(out.created_at).toBe(1700000000)
    expect(out.output).toEqual([
      expect.objectContaining({ type: "reasoning", summary: [{ type: "summary_text", text: "r" }] }),
      expect.objectContaining({ type: "message", content: [{ type: "output_text", text: "Hi", annotations: [] }] }),
      expect.objectContaining({ type: "function_call", call_id: "call_1", name: "t", namespace: "ns", arguments: "{}" }),
      expect.objectContaining({ type: "custom_tool_call", call_id: "call_2", name: "apply_patch", input: "p" }),
    ])
    expect(out.usage).toEqual({ input_tokens: 3, output_tokens: 4, total_tokens: 7 })
  })
})

describe("rewriteOpenAIErrorFramesToResponses", () => {
  it("rewrites only OpenAI error lines, leaving relayed Responses bytes and keepalives untouched", async () => {
    const upstream = [
      ": keepalive",
      'event: response.created\ndata: {"type":"response.created","response":{"id":"resp_1"}}',
      'data: {"error":{"message":"upstream stalled: no data received for 120s","type":"api_error","code":"upstream_stall"}}',
    ]
    const text = await drain(rewriteOpenAIErrorFramesToResponses(sseBody(upstream), "codex/gpt-5.4"))
    expect(text.startsWith(": keepalive\n\nevent: response.created\ndata: {\"type\":\"response.created\"")).toBe(true)
    const failed = events(text).find((e) => e.type === "response.failed")!
    expect(failed).toBeTruthy()
    expect(text).toContain("event: response.failed\ndata: ")
    expect((failed.response as Record<string, unknown>).error).toEqual({
      code: "upstream_stall",
      message: "upstream stalled: no data received for 120s",
    })
    expect((failed.response as Record<string, unknown>).model).toBe("codex/gpt-5.4")
  })
})

describe("collectResponsesSse", () => {
  it("returns the response.completed object", async () => {
    const out = await collectResponsesSse(
      sseBody([
        'data: {"type":"response.created","response":{"id":"resp_1","status":"in_progress"}}',
        'data: {"type":"response.completed","response":{"id":"resp_1","status":"completed","output":[],"usage":{"input_tokens":1,"output_tokens":2}}}',
      ]),
    )
    expect(out).toEqual({
      response: { id: "resp_1", status: "completed", output: [], usage: { input_tokens: 1, output_tokens: 2 } },
    })
  })

  it("reports a failed turn instead of a fabricated success, and an EOF without completion", async () => {
    expect(
      await collectResponsesSse(
        sseBody([
          'data: {"type":"response.failed","response":{"id":"resp_1","status":"failed","error":{"message":"rate limited"}}}',
          'data: {"type":"response.completed","response":{"id":"resp_1","status":"completed","output":[]}}',
        ]),
      ),
    ).toEqual({ error: { message: "rate limited", type: "upstream_error" } })
    expect(await collectResponsesSse(sseBody(['data: {"type":"response.created","response":{}}']))).toEqual({
      error: { message: "upstream ended without response.completed", type: "upstream_error" },
    })
  })
})

describe("buildCodexNativeRequestBody", () => {
  it("applies the Chat-path fix-ups and passes the rest of the CLI body through", async () => {
    const client = codexCliRequest({
      model: "codex/gpt-5.4",
      input: [
        { type: "message", role: "system", content: "sys from client" },
        ...(codexCliRequest().input as unknown[]),
        {
          type: "function_call",
          call_id: "c".repeat(80),
          name: "exec_command",
          arguments: "{}",
        },
        { type: "function_call_output", call_id: "c".repeat(80), output: "ok" },
        { type: "reasoning", id: "rs_1", summary: [], encrypted_content: "gAAAA" },
      ],
      prompt_cache_key: "k".repeat(100),
      max_output_tokens: 50,
      temperature: 0.1,
      previous_response_id: "resp_old",
      service_tier: "flex",
    })
    const body = await buildCodexNativeRequestBody(client, {
      upstreamModel: "gpt-5.4",
      reasoning: { effort: "high", summary: "auto" },
    })
    expect(body.model).toBe("gpt-5.4")
    expect(body.stream).toBe(true)
    expect(body.store).toBe(false)
    expect(body.instructions).toBe("You are Codex.\n\nsys from client")
    const input = body.input as Array<Record<string, unknown>>
    expect(input.some((i) => i.role === "system")).toBe(false)
    // Developer message, reasoning item and hosted tools ride through untouched.
    expect(input[0]).toMatchObject({ role: "developer" })
    expect(input.at(-1)).toMatchObject({ type: "reasoning", encrypted_content: "gAAAA" })
    expect(body.tools).toEqual(client.tools)
    expect(body.client_metadata).toEqual(client.client_metadata)
    // call_id shortened consistently on both halves of the pair.
    const call = input.find((i) => i.type === "function_call")!
    const outputItem = input.find((i) => i.type === "function_call_output")!
    expect(String(call.call_id).length).toBe(64)
    expect(outputItem.call_id).toBe(call.call_id)
    expect(body.include).toEqual(["reasoning.encrypted_content"])
    expect(body.reasoning).toEqual({ effort: "high", summary: "auto" })
    expect(String(body.prompt_cache_key).length).toBe(64)
    expect(body).not.toHaveProperty("max_output_tokens")
    expect(body).not.toHaveProperty("temperature")
    expect(body).not.toHaveProperty("previous_response_id")
    expect(body).not.toHaveProperty("service_tier")
  })

  it("keeps the client's reasoning summary when no effort was sent, and drops tool_choice without tools", async () => {
    const body = await buildCodexNativeRequestBody(
      { model: "x", input: "hi", reasoning: { summary: "auto" }, tool_choice: "auto", parallel_tool_calls: true },
      { upstreamModel: "gpt-5.4" },
    )
    expect(body.reasoning).toEqual({ summary: "auto" })
    expect(body.input).toEqual([{ type: "message", role: "user", content: "hi" }])
    expect(body).not.toHaveProperty("tools")
    expect(body).not.toHaveProperty("tool_choice")
    expect(body).not.toHaveProperty("parallel_tool_calls")
    expect(body).not.toHaveProperty("prompt_cache_key")
    expect(body.include).toEqual(["reasoning.encrypted_content"])
  })

  it("drops effort `none` but keeps the rest of the client's reasoning block", async () => {
    const body = await buildCodexNativeRequestBody(
      { model: "x", input: "hi", reasoning: { effort: "none", summary: "auto" } },
      { upstreamModel: "gpt-5.4" },
    )
    expect(body.reasoning).toEqual({ summary: "auto" })
  })
})

describe("usage capture on a relayed Responses stream", () => {
  it("takes usage from response.completed and counts it as the completion signal", () => {
    const sniffer = createOpenAISseUsageSniffer()
    const enc = new TextEncoder()
    sniffer.feed(enc.encode('data: {"type":"response.output_text.delta","delta":"hi"}\n\n'))
    expect(sniffer.complete()).toBe(false)
    sniffer.feed(
      enc.encode(
        'event: response.completed\ndata: {"type":"response.completed","response":{"usage":{"input_tokens":10,"output_tokens":3,"input_tokens_details":{"cached_tokens":4}}}}\n\n',
      ),
    )
    expect(sniffer.complete()).toBe(true)
    expect(sniffer.finish()).toEqual({
      promptTokens: 10,
      completionTokens: 3,
      cacheReadInputTokens: 4,
      cacheCreationInputTokens: null,
    })
  })

  it("normalizes a Responses usage object on the non-stream path", () => {
    expect(fromOpenAIUsage({ input_tokens: 5, output_tokens: 1, input_tokens_details: { cached_tokens: 2 } })).toEqual({
      promptTokens: 5,
      completionTokens: 1,
      cacheReadInputTokens: 2,
      cacheCreationInputTokens: null,
    })
  })
})
