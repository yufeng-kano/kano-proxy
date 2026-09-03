---
title: Local models (CLI)
description: Expose Ollama, LM Studio, vLLM, or any local OpenAI- or Anthropic-compatible server as a Kano Proxy provider with the kano-proxy CLI.
---

# Local models with the CLI

The `kano-proxy` CLI runs next to a model server on your own machine and connects **outward** to the proxy. Nothing on your machine is exposed to the internet: no port forwarding, no tunnel service, no public URL. The proxy then routes requests for that provider back down the connection.

Works with anything that speaks the OpenAI or Anthropic API on localhost: Ollama, LM Studio, vLLM, llama.cpp server, and others.

## Install

One of:

```bash
brew install yufeng-kano/tap/kano-proxy
```

```bash
curl -fsSL https://raw.githubusercontent.com/yufeng-kano/kano-proxy/main/scripts/install-cli.sh | sh
```

```powershell
scoop bucket add kano https://github.com/yufeng-kano/scoop-bucket
scoop install kano-proxy
```

Later, `kano-proxy update` upgrades the binary in place. Binaries installed by Homebrew or Scoop are upgraded through that manager instead.

## Sign the device in

```console
$ kano-proxy init
  Server: https://<your-domain>
  Device name [my-mac]:
```

The CLI opens the sign-in page in your browser. Approve the device there, then paste the code shown back into the terminal. This is done once per machine. On a headless box, open the printed URL from any browser.

## Register a local server

```console
$ kano-proxy add
  Slug [my-mac]:
  Type: openai
  Target [http://localhost:11434/v1]:
```

- **Slug** becomes the provider prefix: models are called as `my-mac/<model>`.
- **Type** is the API your local server speaks, `openai` or `anthropic`.
- **Target** is the local base URL. For `openai` include the `/v1` (`http://localhost:11434/v1`); for `anthropic` give the origin only (`http://localhost:11434`), because the CLI appends `/v1/messages` itself.

The CLI asks the server for its model list. Keep the default, "All models", and any model you pull later appears automatically. Or pick a subset to expose.

## Start the tunnel

```console
$ kano-proxy start
  my-mac: connected, 3 models reported
```

Leave it running. From any client, anywhere, the local models are now ordinary model ids on your proxy:

```bash
curl https://<your-domain>/openai/v1/chat/completions \
  -H "Authorization: Bearer <your-api-key>" \
  -H "Content-Type: application/json" \
  -d '{"model": "my-mac/llama3.3:70b", "messages": [{"role": "user", "content": "hi"}]}'
```

They also work as targets in a [model group](/guide/endpoints#model-groups), for example as a fallback behind a hosted model.

## Managing devices

- `kano-proxy list` and `kano-proxy status` show what is registered and connected.
- `kano-proxy remove <slug>` deletes a provider.
- The **Providers** page in the app lists devices and lets you revoke one. A revoked device disconnects within the hour and cannot reconnect.

The CLI streams both directions without buffering, reconnects on network failures, and never logs request bodies.
