# Kano Proxy

<p align="center">
  <strong>Turn your AI subscriptions into unified OpenAI- & Anthropic-compatible APIs.</strong>
</p>

<p align="center">
  <a href="./README.md">English</a> · <a href="./README.zh-TW.md">繁體中文</a>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-server-000000?style=flat-square&logo=rust&logoColor=white" alt="Rust" />
  <img src="https://img.shields.io/badge/PostgreSQL-storage-4169E1?style=flat-square&logo=postgresql&logoColor=white" alt="PostgreSQL" />
  <img src="https://img.shields.io/badge/Docker_Compose-self--hosted-2496ED?style=flat-square&logo=docker&logoColor=white" alt="Docker Compose" />
  <img src="https://img.shields.io/badge/OpenAI-Chat_Completions_%26_Responses-00A67E?style=flat-square" alt="OpenAI API" />
  <img src="https://img.shields.io/badge/Anthropic-Messages_API-D97706?style=flat-square&logo=anthropic&logoColor=white" alt="Anthropic API" />
  <img src="https://img.shields.io/badge/Privacy-Zero_Prompt_Logging-059669?style=flat-square" alt="Privacy" />
  <img src="https://img.shields.io/badge/License-MIT-blue?style=flat-square" alt="License" />
</p>

---

**Kano Proxy** is a self-hosted, multi-tenant proxy for developers and coding agents. Bind your personal AI subscription accounts (Claude Code, ChatGPT Codex, SuperGrok, Google AI Pro/Ultra), bring your own OpenAI-/Anthropic-compatible endpoints, or expose the LLM running on your laptop — then pool everything with automatic failover and call it through standard OpenAI and Anthropic APIs.

One Rust server and a PostgreSQL database, run with docker compose on a machine you control. Your upstream OAuth credentials never leave the server; clients only use isolated, project-issued API keys.

> **Want to use it right away?**
> A hosted instance is live at [kano-proxy.yuufeng.com](https://kano-proxy.yuufeng.com).

---

## Key Features

- **Mix & Match Any Model in Any Agent** — Run **GPT & Gemini inside Claude Code**, or run **Claude & Grok inside Cursor, Cline and the Codex CLI**. Full bidirectional protocol translation (Chat Completions, Responses API and Anthropic Messages) means your tools are no longer locked to a single model provider.
- **Multi-Account Pooling & Failover** — Pool multiple accounts per provider. Hit a rate limit (429/403)? Kano benches that account and fails over to the next available one.
- **Model Groups as Virtual Endpoints** — Each group gets its own base URL (`/g/<slug>/openai/v1`, `/g/<slug>/anthropic`) where your model names expand to ordered `provider/model` targets: per-client model mapping and cross-provider failover in one mechanism.
- **Bring Your Own Endpoints** — Register any OpenAI- or Anthropic-compatible API (base URL + key) and it behaves like a built-in provider on both surfaces.
- **Local LLMs, No Public URL** — The `kano-proxy` CLI dials out over WebSocket and serves Ollama, LM Studio, vLLM or llama.cpp on your machine as a first-class provider. No cloudflared, no ngrok, no port forwarding.
- **Usage Dashboard & Spend Limits** — Track 5h and weekly quota per account, see estimated spend per request, and give each API key a daily, weekly or monthly USD ceiling.
- **Privacy by Default** — Prompts and completions are never logged or stored.
- **Built for Coding Agents** — Zero-buffering SSE streaming with cancellation, tool calling, vision, audio input and transcriptions, reasoning effort mapping, and Anthropic prompt caching passthrough.

---

## How It Works

<p align="center">
  <img src="./docs/assets/how-it-works.svg" alt="How Kano Proxy Works" width="100%" />
</p>

1. **Sign In** with Google via the Web UI.
2. **Bind** your subscription accounts (Claude Code, Codex, Grok, Antigravity), add custom endpoints, or connect a local LLM with the CLI.
3. **Generate** a Kano API key (`sk-kano-proxy-...`).
4. **Point** your tools (Claude Code, Codex CLI, Cursor, Cline, Aider, OpenAI SDK) at Kano Proxy.

---

## Quick Start

### 1. Endpoint URLs

| Protocol | Base URL | Supported Clients |
|---|---|---|
| **OpenAI-compatible** | `https://<your-domain>/openai/v1` | Codex CLI, Cursor, Cline, Roo Code, Aider, CC Switch, OpenAI SDK |
| **Anthropic Messages** | `https://<your-domain>/anthropic` | Claude Code CLI, Anthropic SDK, Claude-shaped tools |
| **Model group** | `https://<your-domain>/g/<slug>/openai/v1` · `/g/<slug>/anthropic` | The same clients, with the group's model names |

The OpenAI-compatible base serves **Chat Completions** (`/chat/completions`), the **Responses API** (`/responses`, the wire the Codex CLI speaks), `/models` and `/audio/transcriptions`. Codex models pass through to your ChatGPT subscription natively on it; every other provider is converted on the fly.

### 2. Authentication

```http
Authorization: Bearer sk-kano-proxy-...
```
*(Anthropic clients can also use `x-api-key: sk-kano-proxy-...`)*

### 3. Model IDs

Use standard `provider/model` naming on **both** endpoints:

- `claude-code/claude-opus-5` / `claude-code/claude-sonnet-5`
- `codex/gpt-5.6-sol`
- `grok/grok-4.5`
- `antigravity/gemini-3-flash`
- `<custom-slug>/<model-name>` for a custom endpoint
- `<cli-slug>/<local-model>` for a local LLM served through the CLI

---

## Integration Examples

### Run GPT & Gemini directly in Claude Code

Point the Claude Code CLI at Kano Proxy to use OpenAI or Gemini models with native tool calling:

```bash
export ANTHROPIC_BASE_URL="https://<your-domain>/anthropic"
export ANTHROPIC_API_KEY="sk-kano-proxy-..."

# Run Claude Code powered by GPT or Gemini
claude --model codex/gpt-5.6-sol
# or
claude --model antigravity/gemini-3-flash
```

### Run the Codex CLI through the proxy

Add Kano Proxy as a model provider in `~/.codex/config.toml`. Codex models pass through natively on the CLI's own Responses wire; any other model id from your Models page (`claude-code/...`, `antigravity/...`) works in the same slot:

```toml
model = "codex/gpt-5.6-sol"
model_provider = "kano"
model_catalog_json = "kano-models.json"  # so /model lists codex/... ids; see the docs

[model_providers.kano]
name = "Kano Proxy"
base_url = "https://<your-domain>/openai/v1"
experimental_bearer_token = "<your-api-key>"
wire_api = "responses"
```

### Run Claude in Cursor / OpenAI SDK

Point any OpenAI-compatible client at `/openai/v1`:

```bash
curl https://<your-domain>/openai/v1/chat/completions \
  -H "Authorization: Bearer sk-kano-proxy-..." \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-code/claude-sonnet-5",
    "messages": [{"role": "user", "content": "Write a quicksort in Rust."}],
    "stream": true
  }'
```

### Serve a local Ollama through the proxy

Install the CLI (Homebrew, Scoop or the install script), sign the machine in once, register the local endpoint, and keep the tunnel running:

```bash
curl -fsSL https://raw.githubusercontent.com/yufeng-kano/kano-proxy/main/scripts/install-cli.sh | sh
# or: brew install yufeng-kano/tap/kano-proxy

kano-proxy init     # opens the browser, you approve the device, paste the code
kano-proxy add      # slug, openai|anthropic, http://localhost:11434/v1
kano-proxy start    # one outbound WebSocket per registered provider
```

The local models now answer as `<slug>/<model>` on every base URL above and can be targets in a model group. Details in [docs/cli.md](./docs/cli.md).

---

## Supported Providers

| Provider | Upstream Source | Multi-Account Pool |
|---|---|:---:|
| <img src="https://img.shields.io/badge/Claude_Code-D97706?style=flat-square&logo=anthropic&logoColor=white" alt="Claude Code" height="20" /> | Anthropic Claude Pro / Team OAuth | <img src="https://img.shields.io/badge/Supported-10B981?style=flat-square" alt="Supported" height="18" /> |
| <img src="https://img.shields.io/badge/ChatGPT_Codex-00A67E?style=flat-square" alt="Codex" height="20" /> | ChatGPT Plus / Team / Pro (Codex Backend) | <img src="https://img.shields.io/badge/Supported-10B981?style=flat-square" alt="Supported" height="18" /> |
| <img src="https://img.shields.io/badge/xAI_Grok-000000?style=flat-square&logo=x&logoColor=white" alt="Grok" height="20" /> | xAI SuperGrok OAuth | <img src="https://img.shields.io/badge/Supported-10B981?style=flat-square" alt="Supported" height="18" /> |
| <img src="https://img.shields.io/badge/Google_Antigravity-4285F4?style=flat-square&logo=google&logoColor=white" alt="Antigravity" height="20" /> | Google AI Pro / Ultra (CloudCode API) | <img src="https://img.shields.io/badge/Supported-10B981?style=flat-square" alt="Supported" height="18" /> |
| <img src="https://img.shields.io/badge/Custom_Endpoints-6366F1?style=flat-square&logo=fastapi&logoColor=white" alt="Custom Endpoints" height="20" /> | Any OpenAI- or Anthropic-compatible API | <img src="https://img.shields.io/badge/Supported-10B981?style=flat-square" alt="Supported" height="18" /> |
| <img src="https://img.shields.io/badge/Local_LLMs_(CLI)-0891B2?style=flat-square&logo=ollama&logoColor=white" alt="Local LLMs" height="20" /> | Ollama, LM Studio, vLLM, llama.cpp on your machine, via the `kano-proxy` CLI tunnel | <img src="https://img.shields.io/badge/Supported-10B981?style=flat-square" alt="Supported" height="18" /> |

---

## Self-Hosting & Development

Four containers, one job each: PostgreSQL, the API server, the web app (admin UI plus the public `/docs/` site), and Caddy for TLS and routing.

```bash
git clone https://github.com/yufeng-kano/kano-proxy.git
cd kano-proxy
cp .env.example .env     # fill it in: domain, Google OAuth client, secrets
docker compose up -d --build
```

Point your domain's DNS at the machine and open ports 80 and 443. Caddy fetches a certificate on first request and the server applies its migrations at start. Already running a reverse proxy? Drop the `caddy` service and route the API paths to the server yourself, as described in the deployment doc.

Each app installs and builds on its own — there is no repository-wide package manifest.

```bash
cd apps/api  && cargo test --workspace
cd apps/cli  && cargo test
cd apps/web  && pnpm install && pnpm typecheck && pnpm build
cd apps/docs && pnpm install && pnpm build
```

Configuration, TLS, migrations and releases are in [docs/deployment.md](./docs/deployment.md); the architecture is in [docs/rust-server.md](./docs/rust-server.md).

---

## Documentation

- [Product Goals & Model Naming](./docs/product.md)
- [API Reference & Protocol Mapping](./docs/api.md)
- [Authentication & Account Pools](./docs/auth.md)
- [Providers, Routing & Failover](./docs/providers.md)
- [CLI & Local LLM Tunnel](./docs/cli.md)
- [Deployment](./docs/deployment.md)
- [Full Documentation Map](./docs/index.md)

---

## License

[MIT](./LICENSE)
