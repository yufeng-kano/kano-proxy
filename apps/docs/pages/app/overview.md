---
title: Overview
description: What the Overview page shows in Kano Proxy, spend, requests, tokens, caching and per-model usage for a day, week or month, and how to read it.
---

# Overview

Overview shows how much you used the proxy over one day, week, or month: estimated spend, requests, tokens, caching, and which models did the work. It opens first after you sign in.

## What is on the page

### Time range

The **Day**, **Week**, and **Month** control in the page header sets the range for everything below it.

- **Day** shows one day, one bar per hour.
- **Week** shows one Monday to Sunday week, one bar per day.
- **Month** shows one calendar month, one bar per day.

Choose the selected option again to open a calendar and pick a date, week, or month. Ranges follow your browser's time zone. The refresh button next to the control reloads the numbers.

### Metric cards

Three cards sit at the top: **Spend**, **Requests**, and **Tokens**. Each one shows:

- the total for the range,
- a small stacked chart of that metric over the range, one color per model,
- the top four models by that metric, with the rest folded into **Others**.

**Spend** is an estimate in USD, not a bill. It prices each request's tokens at the model's public API list price, including requests that went through a subscription. The line under the total says how much of it is priced:

- **Estimated from list prices**: every request in the range has a price.
- **Estimated · covers N of M requests**: some models have no known price, so the total is low.
- **No priced requests in this range**: the total shows `—`, not `$0.00`.

**Tokens** counts input plus output tokens.

The expand button on a card opens a larger chart and a table with each model's **Min**, **Max**, **Avg**, and **Sum** per bar over the range. **Avg** ignores bars where that model had no traffic.

### Activity

The card below has four tabs. Its header also shows **Errors**, the number of failed requests, and **Avg latency**, the average of the **Latency** column in [Logs](/app/logs).

| Tab | What it shows |
|-----|---------------|
| **Tokens** | Tokens per bar, stacked by model |
| **Requests** | Requests per bar, stacked by model |
| **Caching** | Input tokens per bar, split into **Cached** and **Uncached** |
| **By model** | One row per model with **Requests**, **Errors**, **Input**, **Cached**, **Cache write**, **Output**, **Cache rate**, and **Spend** |

Models are always shown by their full `provider/model` id. A request sent through a model group counts under the model that actually answered. Requests to every account of a provider add up in the same row. To see which account or group served a single request, use [Logs](/app/logs).

Some providers do not report token counts for every request. Those requests still count in **Requests** and **Errors** but not in token totals. When that happens, **Cache rate** says how many requests it covers, for example `on 40 of 52`.

Usage from providers you have since removed is not shown here. Logs still lists those requests.

## Common tasks

### See what one day cost

1. Choose **Day**.
2. Choose **Day** again to open the calendar.
3. Pick the date.
4. Read **Spend**, then expand it to see the cost per model.

### Check whether prompt caching works

1. Pick a range with traffic.
2. Open the **Caching** tab. A large **Cached** share means repeated prompts are read from cache.
3. Open **By model** and compare **Cache rate** between models.

### Find the cause of a spike

1. Find the tall bar in **Tokens** or **Requests** and hover it to see the models in it.
2. Go to [Logs](/app/logs) and filter by that model's provider to see the single requests.

## Empty page

**No requests yet** means no request in the selected range reached the proxy with your keys. Choose **Get your API key** to create one on the Access page.

## Related pages

- [Logs](/app/logs): every single request, with account, key, and error.
- [Models](/app/models): the model ids you can call.
- [Access](/app/access): API keys and per-key spend limits.
