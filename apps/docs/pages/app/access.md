---
title: Access
description: The Access page holds your credentials. Create and limit API keys, manage the machines signed in with the kano-proxy CLI, and copy the base URLs a client needs.
---

# Access

Access is where your credentials live: the API keys your tools send, the machines signed in with the `kano-proxy` CLI, and the base URLs a client connects to. It has three tabs: **Keys**, **Devices** and **Connect**.

## Keys

A table of your API keys. Every request to the proxy carries one of them.

| Column | Shows |
|--------|-------|
| **Name** | The name you gave the key. A key created by the CLI also reads `from <device name>` under it. |
| **Key** | The first characters of the key, enough to tell keys apart. The full key is never shown again after creation. |
| **Spend** | Estimated spend in the current window. With a limit set it reads spent / limit, for example `$3.20 / $50.00`. |
| **Last used** | When a request last arrived with this key, or **Never**. |

The pencil at the end of a row opens the key for editing. **Create key** sits in the page header while the Keys tab is open.

### Create a key

1. Choose **Create key**.
2. Enter a **Name**, for example the tool or machine that will use it.
3. Optionally set a **Spend limit** (below). Leave it blank for no limit.
4. Choose **Create key**. The dialog switches to **Key created** and shows the full key, which starts with `sk-kano-proxy-`.
5. Copy it now and choose **Done**. It is shown once. After the dialog closes only the prefix remains.

Create one key per tool or machine. Then you can revoke one without touching the others, and the Logs page shows which tool sent what.

### Spend limits

A key's limit has three fields in the create and edit dialogs:

- **Spend limit** is an amount in USD. Blank means no limit.
- **Resets** sets the window: **Daily** (from 00:00 UTC), **Weekly** (from Monday 00:00 UTC), **Monthly** (from the 1st, 00:00 UTC) or **Never (all-time)**. The default is **Monthly**.
- **Count subscription usage** decides whether requests answered by your connected subscriptions (Claude Code, Codex, Grok, Antigravity) count, at their list prices. It is on by default. Custom endpoints always count.

When the key's estimated spend in the window reaches the limit, model requests with that key fail with `429` and the code `spend_limit_exceeded`, before anything is sent upstream. Listing models still works. The limit is checked before each request starts, so the last request that fits can take the total slightly past it. Spend is the proxy's estimate from public prices, not a bill. See [Errors](/reference/errors).

### Edit or revoke a key

1. Choose the pencil on the key's row. The **Edit key** dialog shows the key's current spend in this window.
2. Change the **Name** or the limit fields and choose **Save changes**.
3. To revoke, choose **Revoke** at the bottom left of the dialog and confirm. Anything using the key stops working immediately. This cannot be undone.

## Devices

The **CLI devices** card lists every machine that ran `kano-proxy login` with your account.

| Column | Shows |
|--------|-------|
| **Name** | The device name chosen at sign-in. |
| **Last seen** | When the device last talked to the proxy. |
| **Created** | When it was signed in. Hidden on narrow screens. |
| **Keys** | How many API keys this device created, for example with `kano-proxy agent setup`. |

With no devices yet, the card shows the install commands and the exact `kano-proxy login` command for this instance, plus a link to all platforms on GitHub Releases. See [Install and sign in](/cli/install).

### Rename or revoke a device

1. Choose the pencil in the card's header. Each row gains a rename button and **Revoke**.
2. To rename, choose the row's pencil, change the **Display name** and choose **Done**.
3. To revoke, choose **Revoke** and confirm. The confirmation says how many API keys go with the device.
4. Choose the check mark in the header to leave edit mode.

Revoking deletes the device and every API key it created. It stops connecting within the hour and can only come back by running `kano-proxy login` again on that machine. Local endpoints it registered stay on the Providers page until you remove them. Use this for a lost or retired machine: one action, no hunt through the key list.

### Approving a sign-in

`kano-proxy login` opens a page at `https://<your-domain>/cli/authorize?request=…` in your browser. It asks you to sign in with Google first if you are not signed in. The page is a single card:

1. It asks **Sign in the device "&lt;device name&gt;"?**. Approve only if you just ran `kano-proxy login` on a machine you control.
2. Choose **Approve**. The page shows a code under **Enter this code in your terminal**.
3. Type the code into the CLI. It works once and expires in 10 minutes. Five wrong attempts also end it.

**Deny** ends the request and nothing is signed in. A link that expired or was already used says so; run `kano-proxy login` again for a fresh one.

## Connect

The **Connect a client** card shows what any client needs, for this instance:

- **OpenAI-compatible**: the base URL `https://<your-domain>/openai/v1`, with a copy button.
- **Anthropic-compatible**: the base URL `https://<your-domain>/anthropic`, with a copy button.
- **Protocols**: the OpenAI base serves Chat Completions and the Responses API. The Anthropic base serves Messages.
- **Authorization**: send the key as a bearer token or in the `x-api-key` header.
- Model ids take the form `provider/model`.

The full rules are in [Endpoints and model ids](/reference/endpoints).

## Related pages

- [Logs](/app/logs): every request, with the key that sent it.
- [Groups](/app/groups): endpoints with your own model names, also called with these keys.
- [Install and sign in](/cli/install): the CLI that creates devices.
- [Set up agents](/cli/agents): creates a key per agent and device for you.
