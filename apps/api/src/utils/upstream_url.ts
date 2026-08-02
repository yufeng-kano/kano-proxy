/**
 * SSRF / loop guard for user-supplied custom-provider base URLs.
 * https only, no embedded credentials, no query/fragment, and the hostname
 * must not be localhost, a private/loopback/link-local literal, or this
 * deploy's own host. Applied on create, update, and the test-connection
 * endpoint — never trust a stored value that skipped this check.
 */

export type UpstreamUrlCheckOpts = {
  /** Bare lowercase hostname of the incoming admin request (no port). */
  requestHost?: string | null
  /** Bare lowercase hostname parsed from env.APP_URL, when set. */
  appUrlHost?: string | null
}

export type UpstreamUrlResult = { ok: true; url: string } | { ok: false; error: string }

export function validateUpstreamBaseUrl(
  input: string,
  opts: UpstreamUrlCheckOpts = {},
): UpstreamUrlResult {
  const trimmed = input.trim()
  if (!trimmed) return { ok: false, error: "base_url is required" }

  let url: URL
  try {
    url = new URL(trimmed)
  } catch {
    return { ok: false, error: "base_url must be a valid URL" }
  }

  if (url.protocol !== "https:") {
    return { ok: false, error: "base_url must use https" }
  }
  if (url.username || url.password) {
    return { ok: false, error: "base_url must not contain credentials" }
  }
  if (url.search) {
    return { ok: false, error: "base_url must not contain a query string" }
  }
  if (url.hash) {
    return { ok: false, error: "base_url must not contain a fragment" }
  }

  const hostError = checkHostname(url.hostname.toLowerCase(), opts)
  if (hostError) return { ok: false, error: hostError }

  // Strip trailing slash(es): endpoints are built by literal concatenation
  // (`${base}/chat/completions`), so a stored trailing slash would double up.
  const normalized = url.toString().replace(/\/+$/, "")
  return { ok: true, url: normalized }
}

function checkHostname(hostname: string, opts: UpstreamUrlCheckOpts): string | null {
  if (hostname === "localhost" || hostname.endsWith(".localhost") || hostname.endsWith(".local")) {
    return "base_url must not point at localhost"
  }

  const ipv4 = parseIPv4(hostname)
  if (ipv4 && isPrivateOrLoopbackIPv4(ipv4)) {
    return "base_url must not point at a private or loopback address"
  }
  if (!ipv4 && hostname.startsWith("[") && hostname.endsWith("]")) {
    if (isBlockedIPv6(hostname.slice(1, -1))) {
      return "base_url must not point at a private or loopback address"
    }
  }

  const requestHost = opts.requestHost?.toLowerCase()
  const appUrlHost = opts.appUrlHost?.toLowerCase()
  if ((requestHost && hostname === requestHost) || (appUrlHost && hostname === appUrlHost)) {
    return "base_url must not point at this deploy's own host"
  }

  return null
}

function parseIPv4(hostname: string): [number, number, number, number] | null {
  const m = hostname.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/)
  if (!m) return null
  const parts = [Number(m[1]), Number(m[2]), Number(m[3]), Number(m[4])]
  if (parts.some((n) => n > 255)) return null
  return parts as [number, number, number, number]
}

function isPrivateOrLoopbackIPv4([a, b]: [number, number, number, number]): boolean {
  if (a === 127) return true // 127.0.0.0/8 loopback
  if (a === 0) return true // 0.0.0.0/8 ("this network", includes 0.0.0.0 itself)
  if (a === 10) return true // 10.0.0.0/8
  if (a === 172 && b >= 16 && b <= 31) return true // 172.16.0.0/12
  if (a === 192 && b === 168) return true // 192.168.0.0/16
  if (a === 169 && b === 254) return true // 169.254.0.0/16 link-local
  return false
}

/** hostname is already stripped of the [] brackets URL.hostname keeps for IPv6. */
function isBlockedIPv6(addr: string): boolean {
  const a = addr.toLowerCase()
  if (a === "::1") return true // loopback
  const firstHextet = a.split(":")[0] || ""
  if (/^fe[89ab]/.test(firstHextet)) return true // fe80::/10 link-local
  if (/^f[cd]/.test(firstHextet)) return true // fc00::/7 unique-local
  return false
}
