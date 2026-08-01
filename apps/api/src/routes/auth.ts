import { Hono } from "hono"
import { beginGoogleLogin, completeGoogleLogin } from "../auth/google"
import {
  clearSessionCookie,
  createSession,
  destroySession,
  getCookieSessionId,
  loadSessionUser,
  type HonoEnv,
} from "../auth/session"
import { upsertGoogleUser } from "../db/users"

export const authRoutes = new Hono<HonoEnv>()

authRoutes.get("/login", async (c) => {
  try {
    const { url } = await beginGoogleLogin(c.env)
    return c.redirect(url)
  } catch (e) {
    return c.json({ error: e instanceof Error ? e.message : "login failed" }, 500)
  }
})

authRoutes.get("/callback", async (c) => {
  const code = c.req.query("code")
  const state = c.req.query("state")
  if (!code || !state) return c.text("Missing code/state", 400)
  try {
    const profile = await completeGoogleLogin(c.env, code, state)
    const user = await upsertGoogleUser(c.env.DB, profile)
    const { cookie } = await createSession(c.env, user.id, {
      secure: new URL(c.req.url).protocol === "https:",
    })
    const app = (c.env.APP_URL || "").replace(/\/$/, "")
    return new Response(null, {
      status: 302,
      headers: {
        // Admin SPA lives on APP_URL (Vite in local, Pages in prod). Never leave users on the Worker root.
        location: `${app}/accounts`,
        "set-cookie": cookie,
      },
    })
  } catch (e) {
    return c.text(e instanceof Error ? e.message : "callback failed", 400)
  }
})

authRoutes.post("/logout", async (c) => {
  const sid = getCookieSessionId(c)
  if (sid) await destroySession(c.env, sid)
  return new Response(JSON.stringify({ ok: true }), {
    headers: {
      "content-type": "application/json",
      "set-cookie": clearSessionCookie(new URL(c.req.url).protocol === "https:"),
    },
  })
})

authRoutes.get("/me", async (c) => {
  const loaded = await loadSessionUser(c.env, c.req.header("cookie"))
  if (!loaded) return c.json({ user: null }, 401)
  const u = loaded.user
  return c.json({
    user: {
      id: u.id,
      email: u.email,
      name: u.name,
      picture_url: u.picture_url,
    },
  })
})
