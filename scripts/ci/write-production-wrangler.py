#!/usr/bin/env python3
"""Write apps/api/wrangler.production.toml from CI env (never commit the output).

Required env:
  CF_D1_DATABASE_ID, CF_KV_BENCH_ID, CF_KV_CACHE_ID, APP_URL
Optional env:
  GITHUB_REPO (defaults to yufeng-kano/kano-proxy)
  CODEX_RELAY_URL (codex egress relay origin; empty = var omitted, relay off)
"""

from __future__ import annotations

import os
import sys
from pathlib import Path


def require(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise SystemExit(f"missing required env: {name}")
    return value


def main() -> None:
    app = require("APP_URL").rstrip("/")
    if not app.startswith("https://"):
        raise SystemExit("APP_URL must start with https://")

    d1 = require("CF_D1_DATABASE_ID")
    bench = require("CF_KV_BENCH_ID")
    cache = require("CF_KV_CACHE_ID")
    # Optional — not in require(): CI must not fail for a missing optional var.
    repo = os.environ.get("GITHUB_REPO", "yufeng-kano/kano-proxy").strip()
    relay = os.environ.get("CODEX_RELAY_URL", "").strip().rstrip("/")
    if relay and not relay.startswith("https://"):
        raise SystemExit("CODEX_RELAY_URL must start with https:// when set")
    relay_line = f'CODEX_RELAY_URL = "{relay}"\n' if relay else ""

    root = Path(__file__).resolve().parents[2]
    path = root / "apps" / "api" / "wrangler.production.toml"

    content = f"""name = "kano-proxy"
main = "src/index.ts"
compatibility_date = "2025-02-24"
compatibility_flags = ["nodejs_compat"]

[[d1_databases]]
binding = "DB"
database_name = "kano-proxy"
database_id = "{d1}"
migrations_dir = "migrations"

[[kv_namespaces]]
binding = "BENCH"
id = "{bench}"

[[kv_namespaces]]
binding = "CACHE"
id = "{cache}"

[vars]
APP_URL = "{app}"
GOOGLE_REDIRECT_URI = "{app}/api/auth/callback"
GITHUB_REPO = "{repo}"
{relay_line}
[durable_objects]
bindings = [{{ name = "AGENT_TUNNEL", class_name = "AgentTunnel" }}]

[[migrations]]
tag = "v1"
new_sqlite_classes = ["AgentTunnel"]

[triggers]
crons = ["17 3 * * *"]

[observability]
enabled = true

[limits]
cpu_ms = 15000
"""
    path.write_text(content)
    print(f"wrote {path.relative_to(root)} (resource ids not logged)", file=sys.stderr)


if __name__ == "__main__":
    main()
