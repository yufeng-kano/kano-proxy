---
title: Logs
description: What the Logs page shows in Kano Proxy, one row per request with model, account, API key, status, tokens, cost and latency, how to filter by model or alias, and how to export rows.
---

# Logs

Logs lists every request your API keys sent through the proxy, newest first, one row per request. Use it to see which account served a request, which key sent it, and why it failed.

The proxy never stores prompts or responses. A row holds only metadata: who, which model, the result, token counts, and timing.

## What is on the page

### Filters

The page header has these controls:

- **Model**: **All models**, or one model or alias. The list shows what you used today, with each option's request count and cost. **By model** lists models; **By alias** lists [model group](/app/groups) names plus **No alias**. The switch also changes which name leads in each row.
- **Provider**: **All providers**, or one built-in provider or custom endpoint, with its kind and today's count and cost. Choosing one narrows the model list.
- **Show**: **All** or **Errors**. **Errors** keeps only failed requests.
- **Columns**: choose which columns to show. **Reset** brings back every column at its default width.
- The refresh button loads the newest rows.

Rows are grouped under a heading per day, newest first. The page loads more rows as you scroll. **Load more** at the bottom does the same by hand.

### Columns

| Column | What it shows |
|--------|---------------|
| **Time** | When the request was logged, in your local time. The day is in the heading above |
| **Model** | A colored dot, then the model name and, in grey, the provider and the alias (**via**) if the client called a [model group](/app/groups). Under **By alias**, the alias comes first, then **→** and the model. The dot matches the option in the **Model** list |
| **Account** | Which connected account served the request |
| **API key** | The name of the key that sent it |
| **Status** | The HTTP status. Red when the request failed, including a stream that started with `200` and then failed. Open the row for the error code |
| **Input**, **Cache read**, **Cache write**, **Output** | Token counts the provider reported |
| **Cost** | Estimated USD at the model's API list price. `—` when the model has no known price |
| **Latency** | How long the provider took to answer. For a streamed response, the time until the stream started, not until the last token |

**Time** and **Model** are always shown. Drag the edge of a column header to change its width. Your column choice and widths are saved in this browser, not in your account. Narrow screens hide some columns. Each row is one line; point at a cut-off cell to see the full text.

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
- **Type**: **OAuth** for a subscription account (Claude Code, Codex, Grok, Antigravity), **API** for a custom endpoint or local model.
- **Error**: the error code, if any.

The key's id and secret are never shown.

### Export

Click the arrow next to the **Logs** title and choose **Export…**. In the dialog, choose:

- **Time range**: **Today**, **7 days**, **30 days**, or **Custom** dates.
- **Use the page's filters**: export only what the model, provider and **Show** filters keep.
- **Columns**: the columns shown in the table, or **All fields**, which adds ids, type, upstream status and error code.
- **Format**: **CSV** opens in Excel, Numbers or Google Sheets, with times in your local time zone. **JSON** is one array of objects with raw numbers and UTC times.

The file holds the same metadata as the page. Prompts and responses are never stored, so they are never in it.

## Common tasks

### Find out why a client got an error

1. Set **Show** to **Errors**.
2. Find the request by **Time** and **API key**. Its **Status** is red.
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
