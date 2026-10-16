---
title: Providers
description: What the Providers page in Kano Proxy does, connecting subscription accounts, custom endpoints and local models, reading account status and usage windows, and ordering the account pool.
---

# Providers

Providers is where you connect the accounts and endpoints that answer your requests. Every model you can call comes from something connected here.

There are three kinds:

- **Subscription accounts.** Claude Code, Codex, Grok and Antigravity. You sign in to a subscription you already pay for.
- **Custom endpoints.** Any OpenAI- or Anthropic-compatible API you hold a key for.
- **CLI providers.** A model server on your own machine, connected through the `kano-proxy` CLI.

## What is on the page

### Header

- The tabs narrow the page to one section: **All**, one tab per subscription provider, **Custom** and **CLI**. Each tab shows how many accounts it holds. **All** shows every section stacked.
- The refresh button reloads every section and asks each account for fresh usage. Without it, usage refreshes every 2 minutes while the page is open.

### Subscription accounts

Each provider has its own card with one row per account. Accounts of one provider form a pool: requests for that provider go to the first usable account, and move down the list when it cannot take them. [How requests are routed](/reference/routing) has the full rules.

A row shows:

- **The account name.** The email or username the provider reports, or a name you gave it with Rename. A renamed account still shows the provider's identity underneath.
- **Primary.** The first account in the pool's order. It keeps this badge while it is limited or paused, so you can see that traffic has moved away from it.
- **The plan**, such as `Pro plan`, when the provider reports one.
- **A status**, always with a word beside the dot:

| Status | Meaning |
|--------|---------|
| **Active** | The next request for this provider goes here. |
| **Standby** | Usable. It takes requests when the accounts above it cannot. |
| **Limit reached** | A usage window is at 100%. It is skipped until that window resets. |
| **Paused** | The provider rejected a recent request, for example with a rate limit. It is skipped until the pause ends. |
| **Needs attention** | The stored sign-in no longer works. Remove the account and add it again. |
| **Active · no Fable** | Claude Code only. The next request goes here, but this seat cannot serve Fable models. |
| **Active · Fable** | Claude Code only. Fable requests go here, because the account above it cannot serve them. |

- **Usage bars**, one per usage window the provider reports, such as **5h** and **Week**, each with the percentage used and when it resets. Antigravity shows **Gemini 5h** and **Gemini Week**, then **Argon Week** on Google AI Pro, where Gemini 4 Argon has its own weekly quota, then **Other 5h** and **Other Week**, and a line such as `1,500 credits left` when Google reports a balance. A new account shows **Usage will appear once this account is used**.

### Custom endpoints

One row per custom endpoint: its name, its format (**OpenAI** or **Anthropic**), the **Model prefix** its ids use (such as `mygw/*`), the base URL, a masked key and a status, **Active** or **Paused**. A custom endpoint has no usage windows to show.

### CLI providers

One row per local model server registered from the CLI: its prefix, format, **Connected** or **Offline**, how many models it last reported, and the device it was registered from. There is no add button here. You register a server from the terminal; see [Local models](/cli/local-models).

### Edit mode

Rows show identity and usage only. To change anything, choose the pencil in a section's header (**Edit Claude Code**, **Edit Custom endpoints**, and so on). It reveals the actions on every row of that section. Choose it again, now a check, to close them.

In edit mode a subscription section shows **Strategy** in its header. **Ordered** is the only option: priority order, first usable account.

## Common tasks

### Connect a subscription account

1. Choose the plus button (**Add account**) in the provider's card.
2. Choose **Continue**. The provider's own sign-in page opens in a new tab. If your browser blocked it, choose **Open sign-in page**.
3. Finish the sign-in the way the dialog describes for that provider:
   - **Claude Code.** Approve access, copy the authorization code you are given, paste it into the dialog, then choose **Finish**.
   - **Codex** and **Grok.** Enter the code the dialog shows on the page that opened, then approve. The dialog notices on its own after a few seconds; **I've approved it** checks right away.
   - **Antigravity.** Approve access. Google then sends you to a `localhost` address that does not load. That is expected. Copy that address from the browser's address bar, paste it into the dialog, then choose **Finish**.

The new account goes to the top of the pool and becomes **Primary**. You can connect several accounts of the same provider.

If you already hold tokens for an account, **Enter tokens manually** takes an access token, an optional refresh token and an optional label instead.

### Change the order of a pool

1. Choose the pencil in the provider's card.
2. Use **Move up** and **Move down** on a row. The order is saved at once.

The top row is **Primary** and is tried first. Order matters only between usable accounts: a limited or paused account is skipped wherever it sits.

### Resume a paused account

1. Choose the pencil in the provider's card.
2. Choose the play button (**Resume**) on the paused row.

The account takes requests again right away. If the provider rejects it again, it is paused again. Resume does not appear for **Limit reached**: a full usage window is a limit set by the provider, and only its reset clears it.

### Rename or remove an account

In edit mode, the pencil on a row (**Rename**) sets a display name. Leave the name blank to go back to the provider's own. Renaming changes nothing about routing.

The trash button (**Remove**) asks for confirmation, then deletes the account. Requests stop using it immediately. Adding it back means signing in again.

### Add a custom endpoint

1. In the **Custom** card, choose the plus button (**Add endpoint**).
2. Choose the **API format**: **OpenAI** or **Anthropic**. It cannot be changed later.
3. Enter a **Name**. The **Model prefix** is filled from it. Your models from this endpoint are called `<prefix>/<model>`.
4. Enter the **Base URL**. It must start with `https://`. The line under the field shows the exact address requests go to:
   - OpenAI format: include `/v1` yourself, for example `https://api.example.com/v1`. Requests go to `<base>/chat/completions`.
   - Anthropic format: leave `/v1` out. Requests go to `<base>/v1/messages`.
5. Enter the **API key**. It is stored encrypted and never shown again.
6. Under **Models**, choose **Load automatically** to read the list from the endpoint, or **Enter manually** and type one model id per line.
7. Optionally choose **Test connection** to check the address and key before saving.
8. Choose **Add endpoint**.

A base URL that points at a private network address, `localhost`, or this proxy's own host is rejected. For a server on your own machine, use the CLI instead.

**Token-count URL** appears only for the OpenAI format. Claude Code asks for a token count, and an OpenAI-compatible API has no such endpoint. If your gateway also serves an Anthropic-compatible `/v1/messages/count_tokens`, put its full address here. It is used exactly as typed.

You can add up to 20 custom endpoints and CLI providers together.

### Edit, test or remove a custom endpoint

In edit mode each custom row has:

- **Test**, which sends a probe to the endpoint and shows the result on the row.
- The pencil (**Edit**), which opens the same form. Leave **API key** blank to keep the stored key.
- The trash button (**Remove**), which deletes the endpoint and its key after you confirm. Requests to `<prefix>/…` stop resolving.
- **Move up** and **Move down**, which change only the display order on this page. They do not affect routing.

Changing the **Model prefix** of an existing endpoint opens a confirmation first. It lists the group targets that will be rewritten to the new prefix. Clients that still call the old prefix fail with an invalid model error until you update them.

## Related pages

- [Models](/app/models): every model id the connected providers offer.
- [Groups](/app/groups): your own model names with a failover order across providers and accounts.
- [How requests are routed](/reference/routing): how a request picks an account, and when it moves on.
- [Local models](/cli/local-models): connect a model server on your own machine.
