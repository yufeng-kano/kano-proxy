---
title: Local models
description: Expose Ollama, LM Studio, vLLM, llama.cpp or any local OpenAI- or Anthropic-compatible server as a Kano Proxy provider with kano-proxy tunnel, without a public address.
---

# Local models

`kano-proxy tunnel` makes a model server on your own machine a provider on the proxy. The CLI connects **outward** to the proxy, and requests for that provider come back down the same connection. Nothing on your machine is exposed: no port forwarding, no tunnel service, no public URL.

It works with anything that speaks the OpenAI or Anthropic API on your machine: Ollama, LM Studio, vLLM, llama.cpp server and others.

The machine must be signed in first ([Install and sign in](/cli/install)).

## 1. Register the local server

```bash
kano-proxy tunnel add
```

The screen lists five settings with their defaults. Enter opens one to change it, and **Tab** on the list registers the server. The keys work as in [agent setup](/cli/agents#choosing-models).

- **Slug**: the provider's name on the proxy. Models are then called `<slug>/<model>`. It defaults to the machine's host name, for example `my-mac`. The slug cannot be changed later.
- **API**: what the local server speaks, `openai` or `anthropic`.
- **Target**: the local server's base URL. The screen shows the right form for the API you picked, with examples. The CLI adds the rest of the path itself.
- **Local API key**: only if your local server requires one. Most do not. It stays on this machine; the proxy never sees it.
- **Models**: which models to expose (below).

Common targets:

| Server | `openai` target | `anthropic` target |
|--------|-----------------|--------------------|
| Ollama | `http://localhost:11434/v1` | `http://localhost:11434` |
| LM Studio | `http://localhost:1234/v1` | |
| vLLM | `http://localhost:8000/v1` | |
| llama.cpp server | `http://localhost:8080/v1` | `http://localhost:8080` |

For `openai` the target includes `/v1`; for `anthropic` it is the origin only.

The **Models** item asks the local server for its model list. Keep the first choice, **All models (follow local server)**, and any model you add locally later appears on the proxy by itself. Or pick a subset to expose. If the local server is not running yet, type the model ids by hand; the real list replaces them on the first connect.

You can register several local servers. Each becomes its own provider.

::: details Without the interactive screen
```bash
kano-proxy tunnel add --no-tui --slug my-mac --format openai --target http://localhost:11434/v1
```

`--expose m1,m2` limits the models; without it every model is exposed. Pass a local API key in the `KANO_PROXY_TARGET_KEY` environment variable rather than `--target-key`, which would land in your shell history.
:::

## 2. Start the tunnel

```bash
kano-proxy tunnel start
```

It opens one connection per registered local server and prints one line per connection and per request. Leave it running; it runs in the foreground. Stop it with Ctrl-C. To keep it running in the background, run it from your own launchd or systemd service; the CLI does not start one itself.

While it runs, the local models are ordinary model ids on your proxy, from any client, anywhere:

```bash
curl https://<your-domain>/openai/v1/chat/completions \
  -H "Authorization: Bearer <your-api-key>" \
  -H "Content-Type: application/json" \
  -d '{"model": "my-mac/llama3.3:70b", "messages": [{"role": "user", "content": "hi"}]}'
```

`my-mac/llama3.3:70b` is an example; use a slug and model from your own Models page. Local models also work as targets in a [model group](/app/groups), for example as a fallback behind a hosted model.

## How it behaves

- **New local models appear by themselves.** While connected, the CLI checks the local model list every 5 minutes and reports changes.
- **Network drops reconnect.** The CLI retries with growing pauses, up to one minute apart.
- **A closed laptop does not break failover.** When the machine is offline, requests skip this provider for 60 seconds and move to the next target in a group. Reconnecting makes it usable again at once.
- **At most 4 requests at a time per local server.** More are refused right away, so a group can try its next target instead of waiting. `--concurrency 1` to `3` lowers the cap.
- **Streaming passes through as it arrives**, and a cancelled request is cancelled on the local server too.
- **Nothing is logged but the request line**: method, path, status and duration. Never bodies or headers.

The proxy and the CLI only forward to the target you registered, and only the model API paths. The tunnel cannot reach anything else on your machine.

## Manage local servers

- `kano-proxy tunnel list` shows this machine's local servers: slug, API, target, connection state, number of models and when they were last reported.
- `kano-proxy tunnel remove <slug>` removes one from the proxy and from this machine. `--local-only` only forgets it on this machine, for one already removed in the app.
- `kano-proxy tunnel attach <slug> --target <url>` serves a provider registered on another machine from this one, for example after replacing a computer.

The [Providers](/app/providers) page lists every local server under **CLI providers**, with its state, model count and the device it came from. There you can rename or remove one. A local model counts as a provider: it appears on [Models](/app/models), in [Logs](/app/logs) and in the group target picker.

To sign the machine out, see [Sign out](/cli/install#sign-out). Its local servers stay registered, shown as offline, until you remove them.

## Related pages

- [Providers](/app/providers)
- [Groups](/app/groups)
- [How requests are routed](/reference/routing)
