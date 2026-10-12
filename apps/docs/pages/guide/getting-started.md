---
title: Getting started
description: Sign in, connect a provider, set up a coding agent with the kano-proxy CLI or send a request with curl, and find it on the Logs page.
---

# Getting started

Sign in, connect one provider, point one tool at the proxy, and check that the request arrived. It takes a few minutes.

## 1. Sign in

Open `https://<your-domain>/login` and choose **Continue with Google**. There is no password. The providers, keys, and logs you see belong to your account.

## 2. Connect a provider

1. Open **Providers** in the sidebar.
2. Pick a provider, for example Claude Code, and choose **Add account**.
3. Sign in to that provider in the window that opens. This is the provider's own sign-in, for a subscription you already pay for.

The account then appears on **Providers** with its usage. You can add more than one account per provider; when one runs out, requests move to the next. Custom endpoints and local models are connected on the same page. See [Providers](/app/providers).

## 3. Set up a coding agent

The quickest way is the `kano-proxy` CLI. It creates a key and writes the tool's configuration for you. It sets up Claude Code and Codex CLI.

1. Install the CLI. See [Install and sign in](/cli/install).
2. Sign this machine in. The CLI opens a page in your browser. Approve the device there, then paste the one-time code it shows back into the terminal.

   ```bash
   kano-proxy login
   ```

3. Set up the tool. Use `codex` instead of `claude-code` for Codex CLI.

   ```bash
   kano-proxy agent setup claude-code
   ```

4. Start the tool as usual.

The CLI asks which models answer each of the tool's model slots. [Set up agents](/cli/agents) explains the questions and how to undo the change. For Cursor, Cline, OpenCode, and other tools, follow the tool's own page under **Coding agents**.

## Or call the API directly

1. Open **Access** and choose **Create key**. Copy the key when it appears. It starts with `sk-kano-proxy-` and is shown only once.
2. Pick a model id on **Models**. Ids look like `provider/model`.
3. Send a request. The model id below is an example; use one from your own list.

```bash
curl https://<your-domain>/openai/v1/chat/completions \
  -H "Authorization: Bearer <your-api-key>" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-code/claude-opus-5",
    "messages": [{"role": "user", "content": "Say hello in five words."}],
    "max_tokens": 32
  }'
```

The same key and model id work on the Anthropic base:

```bash
curl https://<your-domain>/anthropic/v1/messages \
  -H "x-api-key: <your-api-key>" \
  -H "anthropic-version: 2023-06-01" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-code/claude-opus-5",
    "max_tokens": 32,
    "messages": [{"role": "user", "content": "Say hello in five words."}]
  }'
```

All URLs, headers, and id rules are in [Endpoints and model ids](/reference/endpoints).

## 4. Check it worked

Open **Logs**. Your request appears at the top within seconds, with the model, the account that answered, the key that sent it, its status, the tokens it used, and its estimated cost. If the request failed, the status and [Errors](/reference/errors) tell you why.

## Next

- [The app](/app/overview): what each page shows and does.
- [How requests are routed](/reference/routing): how the proxy picks an account and what happens when one runs out.
- [Groups](/app/groups): give a tool its own model names, or fall back across providers.
