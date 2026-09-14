import type { HonoRequest } from "hono"

/** Keep parsed JSON reusable without retaining a second, potentially huge raw-text body. */
export async function readProxyJson(request: HonoRequest): Promise<Record<string, unknown>> {
  const body = await request.json<Record<string, unknown>>()
  delete request.bodyCache.text
  request.bodyCache.json = Promise.resolve(body)
  return body
}
