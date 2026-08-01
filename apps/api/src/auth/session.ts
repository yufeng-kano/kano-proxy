import type { Context } from "hono"
import type { Env } from "../env"
import { findUserById, type UserRow } from "../db/users"
import { newId, nowIso } from "../utils/id"

const COOKIE = "kano-proxy_session"
const SESSION_DAYS = 14

export type AppVariables = {
  user: UserRow | null
  apiKeyUserId: string | null
  apiKeyId: string | null
}

export type HonoEnv = { Bindings: Env; Variables: AppVariables }

function signPayload(secret: string, payload: string): Promise<string> {
  return hmac(secret, payload)
}

async function hmac(secret: string, data: string): Promise<string> {
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  )
  const sig = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(data))
  return [...new Uint8Array(sig)].map((b) => b.toString(16).padStart(2, "0")).join("")
}

/**
 * Constant-time string equality — length check first, then XOR-accumulate
 * over every char code regardless of where a mismatch occurs, so response
 * timing cannot be used to guess a valid signature byte-by-byte.
 */
export function timingSafeEqual(a: string, b: string): boolean {
  if (a.length !== b.length) return false
  let diff = 0
  for (let i = 0; i < a.length; i++) {
    diff |= a.charCodeAt(i) ^ b.charCodeAt(i)
  }
  return diff === 0
}

export async function createSession(
  env: Env,
  userId: string,
  opts?: { secure?: boolean },
): Promise<{ id: string; cookie: string }> {
  const secret = env.SESSION_SECRET
  if (!secret) throw new Error("SESSION_SECRET is not configured")
  const id = newId("sess")
  const expires = new Date(Date.now() + SESSION_DAYS * 864e5).toISOString()
  await env.DB.prepare(
    `INSERT INTO sessions (id, user_id, expires_at, created_at) VALUES (?, ?, ?, ?)`,
  )
    .bind(id, userId, expires, nowIso())
    .run()
  const sig = await signPayload(secret, id)
  const cookie = `${COOKIE}=${id}.${sig}; Path=/; HttpOnly; SameSite=Lax; Max-Age=${SESSION_DAYS * 86400}${opts?.secure ? "; Secure" : ""}`
  return { id, cookie }
}

export async function destroySession(env: Env, sessionId: string): Promise<void> {
  await env.DB.prepare(`DELETE FROM sessions WHERE id = ?`).bind(sessionId).run()
}

export function clearSessionCookie(secure?: boolean): string {
  return `${COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0${secure ? "; Secure" : ""}`
}

export async function loadSessionUser(
  env: Env,
  cookieHeader: string | undefined,
): Promise<{ user: UserRow; sessionId: string } | null> {
  if (!cookieHeader || !env.SESSION_SECRET) return null
  const match = cookieHeader.match(/(?:^|;\s*)kano-proxy_session=([^;]+)/)
  if (!match) return null
  const [sessionId, sig] = match[1]!.split(".")
  if (!sessionId || !sig) return null
  const expect = await signPayload(env.SESSION_SECRET, sessionId)
  if (!timingSafeEqual(expect, sig)) return null
  const row = await env.DB.prepare(
    `SELECT user_id, expires_at FROM sessions WHERE id = ?`,
  )
    .bind(sessionId)
    .first<{ user_id: string; expires_at: string }>()
  if (!row) return null
  if (new Date(row.expires_at).getTime() < Date.now()) {
    await destroySession(env, sessionId)
    return null
  }
  const user = await findUserById(env.DB, row.user_id)
  if (!user) return null
  return { user, sessionId }
}

export function getCookieSessionId(c: Context<HonoEnv>): string | null {
  const cookieHeader = c.req.header("cookie")
  if (!cookieHeader) return null
  const match = cookieHeader.match(/(?:^|;\s*)kano-proxy_session=([^;]+)/)
  if (!match) return null
  return match[1]!.split(".")[0] ?? null
}
