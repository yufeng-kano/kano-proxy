import type { Env } from "../env"
import { newId, nowIso } from "../utils/id"

export type UserRow = {
  id: string
  google_sub: string
  email: string
  name: string | null
  picture_url: string | null
  created_at: string
  updated_at: string
}

export async function findUserByGoogleSub(
  db: D1Database,
  googleSub: string,
): Promise<UserRow | null> {
  return (
    (await db
      .prepare("SELECT * FROM users WHERE google_sub = ?")
      .bind(googleSub)
      .first<UserRow>()) ?? null
  )
}

export async function findUserById(db: D1Database, id: string): Promise<UserRow | null> {
  return (await db.prepare("SELECT * FROM users WHERE id = ?").bind(id).first<UserRow>()) ?? null
}

export async function upsertGoogleUser(
  db: D1Database,
  profile: { sub: string; email: string; name?: string; picture?: string },
): Promise<UserRow> {
  const existing = await findUserByGoogleSub(db, profile.sub)
  const ts = nowIso()
  if (existing) {
    await db
      .prepare(
        `UPDATE users SET email = ?, name = ?, picture_url = ?, updated_at = ? WHERE id = ?`,
      )
      .bind(profile.email, profile.name ?? null, profile.picture ?? null, ts, existing.id)
      .run()
    return { ...existing, email: profile.email, name: profile.name ?? null, picture_url: profile.picture ?? null, updated_at: ts }
  }
  const id = newId("usr")
  await db
    .prepare(
      `INSERT INTO users (id, google_sub, email, name, picture_url, created_at, updated_at)
       VALUES (?, ?, ?, ?, ?, ?, ?)`,
    )
    .bind(id, profile.sub, profile.email, profile.name ?? null, profile.picture ?? null, ts, ts)
    .run()
  return {
    id,
    google_sub: profile.sub,
    email: profile.email,
    name: profile.name ?? null,
    picture_url: profile.picture ?? null,
    created_at: ts,
    updated_at: ts,
  }
}

export async function requireEnvSecret(env: Env, name: keyof Env): Promise<string> {
  const v = env[name]
  if (typeof v !== "string" || !v) {
    throw new Error(`${String(name)} is not configured`)
  }
  return v
}
