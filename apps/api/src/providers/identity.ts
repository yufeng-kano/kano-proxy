/** Resolve email / display name after OAuth for account pool labels. */

export async function fetchClaudeIdentity(accessToken: string): Promise<{
  email: string | null
  displayName: string | null
}> {
  try {
    const res = await fetch("https://api.anthropic.com/api/oauth/profile", {
      headers: {
        authorization: `Bearer ${accessToken}`,
        "anthropic-beta": "oauth-2025-04-20",
        "anthropic-version": "2023-06-01",
      },
    })
    if (!res.ok) return { email: null, displayName: null }
    const json = (await res.json()) as {
      account?: { email?: string; display_name?: string }
    }
    const displayName = json.account?.display_name ?? null
    return {
      email: displayName || json.account?.email ?? null,
      displayName: displayName,
    }
  } catch {
    return { email: null, displayName: null }
  }
}

export async function fetchCodexIdentity(
  accessToken: string,
  accountId: string | null,
): Promise<{ email: string | null; displayName: string | null; plan: string | null }> {
  const fromJwt = emailFromJwt(accessToken)
  if (!accountId) {
    return { email: fromJwt, displayName: null, plan: null }
  }
  try {
    const { fetchCodexUsageJson } = await import("./codex_usage")
    const result = await fetchCodexUsageJson(accessToken, accountId)
    if (result.ok && result.payload) {
      return {
        email: result.payload.email ?? fromJwt,
        displayName: null,
        plan: result.payload.plan_type ?? null,
      }
    }
    return { email: fromJwt, displayName: null, plan: null }
  } catch {
    return { email: fromJwt, displayName: null, plan: null }
  }
}

export async function fetchGrokIdentity(accessToken: string): Promise<{
  email: string | null
  displayName: string | null
}> {
  try {
    const res = await fetch("https://cli-chat-proxy.grok.com/v1/user", {
      headers: {
        authorization: `Bearer ${accessToken}`,
        accept: "application/json",
      },
    })
    if (!res.ok) return { email: null, displayName: null }
    const json = (await res.json()) as {
      email?: string
      name?: string
      username?: string
      preferred_username?: string
    }
    const displayName = json.name ?? json.username ?? json.preferred_username ?? null
    return {
      email: displayName || json.email ?? null,
      displayName: displayName,
    }
  } catch {
    return { email: null, displayName: null }
  }
}

function emailFromJwt(accessToken: string): string | null {
  try {
    const mid = accessToken.split(".")[1]
    if (!mid) return null
    const padded = mid + "=".repeat((4 - (mid.length % 4)) % 4)
    const json = JSON.parse(atob(padded.replace(/-/g, "+").replace(/_/g, "/"))) as Record<
      string,
      unknown
    >
    const direct =
      (typeof json.email === "string" && json.email) ||
      (typeof json.preferred_username === "string" && json.preferred_username) ||
      (typeof json.name === "string" && json.name) ||
      null
    if (direct) return direct
    const auth = json["https://api.openai.com/profile"] as { email?: string } | undefined
    if (auth?.email) return auth.email
    const nested = json["https://api.openai.com/auth"] as { email?: string } | undefined
    if (nested?.email) return nested.email
    return null
  } catch {
    return null
  }
}

export function pickAccountLabel(parts: {
  email?: string | null
  displayName?: string | null
  fallback: string
}): string {
  return parts.email || parts.displayName || parts.fallback
}
