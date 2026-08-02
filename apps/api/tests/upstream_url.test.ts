import { describe, expect, it } from "vitest"
import { validateUpstreamBaseUrl } from "../src/utils/upstream_url"

describe("validateUpstreamBaseUrl", () => {
  it("accepts a plain https URL and passes it through unchanged", () => {
    const res = validateUpstreamBaseUrl("https://api.example.com/v1")
    expect(res).toEqual({ ok: true, url: "https://api.example.com/v1" })
  })

  it("strips a single trailing slash on save", () => {
    const res = validateUpstreamBaseUrl("https://api.example.com/v1/")
    expect(res).toEqual({ ok: true, url: "https://api.example.com/v1" })
  })

  it("strips multiple trailing slashes on save", () => {
    const res = validateUpstreamBaseUrl("https://api.example.com/v1//")
    expect(res).toEqual({ ok: true, url: "https://api.example.com/v1" })
  })

  it("strips the trailing slash a bare origin gets from URL normalization", () => {
    const res = validateUpstreamBaseUrl("https://api.example.com")
    expect(res).toEqual({ ok: true, url: "https://api.example.com" })
  })

  it("rejects empty input", () => {
    const res = validateUpstreamBaseUrl("")
    expect(res.ok).toBe(false)
  })

  it("rejects a non-URL string", () => {
    const res = validateUpstreamBaseUrl("not a url")
    expect(res.ok).toBe(false)
  })

  it("rejects http", () => {
    const res = validateUpstreamBaseUrl("http://api.example.com/v1")
    expect(res).toEqual({ ok: false, error: "base_url must use https" })
  })

  it("rejects other schemes", () => {
    expect(validateUpstreamBaseUrl("ftp://api.example.com/v1").ok).toBe(false)
  })

  it("rejects embedded credentials", () => {
    const res = validateUpstreamBaseUrl("https://user:pass@api.example.com/v1")
    expect(res).toEqual({ ok: false, error: "base_url must not contain credentials" })
  })

  it("rejects a query string", () => {
    const res = validateUpstreamBaseUrl("https://api.example.com/v1?x=1")
    expect(res).toEqual({ ok: false, error: "base_url must not contain a query string" })
  })

  it("rejects a fragment", () => {
    const res = validateUpstreamBaseUrl("https://api.example.com/v1#frag")
    expect(res).toEqual({ ok: false, error: "base_url must not contain a fragment" })
  })

  it("rejects localhost", () => {
    expect(validateUpstreamBaseUrl("https://localhost/v1").ok).toBe(false)
  })

  it("rejects a *.localhost hostname", () => {
    expect(validateUpstreamBaseUrl("https://foo.localhost/v1").ok).toBe(false)
  })

  it("rejects a *.local hostname", () => {
    expect(validateUpstreamBaseUrl("https://my-box.local/v1").ok).toBe(false)
  })

  it("rejects 127.x loopback", () => {
    expect(validateUpstreamBaseUrl("https://127.0.0.1/v1").ok).toBe(false)
    expect(validateUpstreamBaseUrl("https://127.10.20.30/v1").ok).toBe(false)
  })

  it("rejects 0.0.0.0", () => {
    expect(validateUpstreamBaseUrl("https://0.0.0.0/v1").ok).toBe(false)
  })

  it("rejects 10.x", () => {
    expect(validateUpstreamBaseUrl("https://10.1.2.3/v1").ok).toBe(false)
  })

  it("rejects 172.16-31.x but not the rest of 172.x", () => {
    expect(validateUpstreamBaseUrl("https://172.16.0.1/v1").ok).toBe(false)
    expect(validateUpstreamBaseUrl("https://172.31.255.255/v1").ok).toBe(false)
    expect(validateUpstreamBaseUrl("https://172.15.0.1/v1").ok).toBe(true)
    expect(validateUpstreamBaseUrl("https://172.32.0.1/v1").ok).toBe(true)
  })

  it("rejects 192.168.x", () => {
    expect(validateUpstreamBaseUrl("https://192.168.1.1/v1").ok).toBe(false)
  })

  it("rejects 169.254.x link-local", () => {
    expect(validateUpstreamBaseUrl("https://169.254.169.254/v1").ok).toBe(false)
  })

  it("rejects ::1 IPv6 loopback", () => {
    expect(validateUpstreamBaseUrl("https://[::1]/v1").ok).toBe(false)
  })

  it("rejects fe80::/10 IPv6 link-local", () => {
    expect(validateUpstreamBaseUrl("https://[fe80::1]/v1").ok).toBe(false)
  })

  it("rejects fc00::/7 IPv6 unique-local", () => {
    expect(validateUpstreamBaseUrl("https://[fc00::1]/v1").ok).toBe(false)
    expect(validateUpstreamBaseUrl("https://[fd12::1]/v1").ok).toBe(false)
  })

  it("allows a public IPv6 literal", () => {
    expect(validateUpstreamBaseUrl("https://[2001:db8::1]/v1").ok).toBe(true)
  })

  it("rejects the deploy's own request host", () => {
    const res = validateUpstreamBaseUrl("https://kano.example.com/v1", {
      requestHost: "kano.example.com",
    })
    expect(res).toEqual({ ok: false, error: "base_url must not point at this deploy's own host" })
  })

  it("rejects the deploy's own APP_URL host", () => {
    const res = validateUpstreamBaseUrl("https://kano.example.com/v1", {
      appUrlHost: "kano.example.com",
    })
    expect(res.ok).toBe(false)
  })

  it("own-host comparison is case-insensitive", () => {
    const res = validateUpstreamBaseUrl("https://KANO.example.com/v1", {
      requestHost: "kano.example.com",
    })
    expect(res.ok).toBe(false)
  })

  it("allows a different host even when requestHost/appUrlHost are set", () => {
    const res = validateUpstreamBaseUrl("https://upstream.example.com/v1", {
      requestHost: "kano.example.com",
      appUrlHost: "kano.example.com",
    })
    expect(res).toEqual({ ok: true, url: "https://upstream.example.com/v1" })
  })

  it("a valid https URL with no request context passes", () => {
    const res = validateUpstreamBaseUrl("https://openrouter.example.com/api/v1")
    expect(res).toEqual({ ok: true, url: "https://openrouter.example.com/api/v1" })
  })
})
