---
title: Logs
description: What the Logs page shows in Kano Proxy, one row per request with model, account, API key, status, tokens, cost and latency, and how to use it to find errors.
---

# Logs

Logs lists every request your API keys sent through the proxy, newest first, one row per request. Use it to see which account served a request, which key sent it, and why it failed.

The proxy never stores prompts or responses. A row holds only metadata: who, which model, the result, token counts, and timing.

## What is on the page

### Filters

The page header has three controls:

- **Provider**: **All providers**, or one built-in provider or custom endpoint.
- **Show**: **All** or **Errors**. **Errors** keeps only failed requests.
- The refresh button loads the newest rows.

The page loads more rows as you scroll. **Load more** at the bottom does the same by hand.

### Columns

| Column | What it shows |
|--------|---------------|
| **Time** | When the request was logged, in your local time |
| **Model** | The `provider/model` id that answered. **via** and a group name mean the client called a [model group](/app/groups) |
| **Account** | Which connected account served the request |
| **API key** | The name of the key that sent it |
| **Type** | **OAuth** for a subscription account (Claude Code, Codex, Grok, Antigravity), **API** for a custom endpoint or local model |
| **Status** | The HTTP status on success. On failure, the error code in red, for example `upstream_unavailable` |
| **Input**, **Cache read**, **Cache write**, **Output** | Token counts the provider reported |
| **Cost** | Estimated USD at the model's API list price. `—` when the model has no known price |
| **Latency** | How long the provider took to answer. For a streamed response, the time until the stream started, not until the last token |

Narrow screens hide some columns. Long values are cut to two lines; point at a cell to see the full text.

Labels next to a name:

- **Shared**: the account belongs to someone else who shares it with you.
- **Removed**: the key or account was deleted after the request ran. The row keeps the name it had then.
- **Account removed** or **Key removed**: the same, for older rows that did not store the name.

### Request detail

Click a row to open **Request detail**. It shows every field of the row, plus:

- **Request id**, the row's own id.
- **Provider** and **Alias**: the provider, and the group model name the client sent, if any.
- The account's id under its name, which still identifies an account you removed.
- **Upstream status**: the last HTTP status the provider returned. A streamed response always logs `200` in **Status** once it starts, so this is where the provider's real error shows.
- **Error**: the error code, if any.

The key's id and secret are never shown.

## Common tasks

### Find out why a client got an error

1. Set **Show** to **Errors**.
2. Find the request by **Time** and **API key**.
3. Open it and read **Error** and **Upstream status**. [Errors](/reference/errors) explains each code.

### Check which account answered

1. Find the request and read **Account**.
2. If it is not the account you expected, the accounts above it were limited or paused at that moment. See [Providers](/app/providers) for the order.

### Check that a new tool is connected

1. Send one request from the tool.
2. Choose refresh. The request appears within seconds, with the key name you gave the tool.

## How long rows are kept

Old rows are deleted automatically, after 90 days unless the instance is configured otherwise.

## Related pages

- [Overview](/app/overview): the same traffic added up per model.
- [Providers](/app/providers): account order and status.
- [Errors](/reference/errors): what each error code means.
