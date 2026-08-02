#!/usr/bin/env python3
"""Write apps/api/wrangler.production.toml from CI env (never commit the output).

Required env:
  CF_D1_DATABASE_ID, CF_KV_BENCH_ID, CF_KV_CACHE_ID, APP_URL
Optional env:
  GITHUB_REPO (defaults to yufeng-kano/kano-proxy)
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
CODEX_REDIRECT_URI = "http://localhost:1455/auth/callback"
GITHUB_REPO = "{repo}"

[triggers]
crons = ["17 3 * * *"]
"""
    path.write_text(content)
    print(f"wrote {path.relative_to(root)} (resource ids not logged)", file=sys.stderr)


if __name__ == "__main__":
    main()
