---
title: Providers
description: What the Providers page in Kano Proxy does, connecting subscription accounts, custom endpoints and local models, reading account status and usage windows, ordering the account pool, and removing fast mode or the Claude Code line from requests.
---

# Providers

Providers is where you connect the accounts and endpoints that answer your requests. Every model you can call comes from something connected here.

There are three kinds:

- **Subscription accounts.** Claude Code, Codex, Grok and Antigravity. You sign in to a subscription you already pay for.
- **Custom endpoints.** Any OpenAI- or Anthropic-compatible API you hold a key for.
- **CLI providers.** A model server on your own machine, connected through the `kano-proxy` CLI.

## What is on the page

### Header

- The tabs narrow the page to one section: **All**, one tab per subscription provider, **Custom** and **CLI**. **All** shows every section stacked. Each tab shows how many rows its section holds; **All** shows the total once every section has loaded.
- The refresh button reloads every section and asks each account for fresh usage. Without it, usage refreshes every 2 minutes while the page is open.

### Subscription accounts

Each provider has its own card with one row per account. Accounts of one provider form a pool: requests for that provider go to the first usable account, and move down the list when it cannot take them. [How requests are routed](/reference/routing) has the full rules.

A row shows:

- **The account name.** The email or username the provider reports, or a name you gave it with Rename. A renamed account still shows the provider's identity underneath.
- **Primary.** The first account in the pool's order. It keeps this badge while it is limited or paused, so you can see that traffic has moved away from it.
- **The plan**, such as `Pro plan`, when the provider reports one. Claude uses Anthropic's names since October 30, 2026: the old Max 5x reads `Max plan` and the old Max 20x reads `Ultra plan`.
- **A status**, always with a word beside the dot:

| Status | Meaning |
|--------|---------|
| **Active** | The next request for this provider goes here. |
| **Standby** | Usable. It takes requests when the accounts above it cannot. |
| **Limit reached** | A usage window is at 100%. It is skipped until that window resets. |
| **Paused** | The provider rejected a recent request, for example with a rate limit. It is skipped until the pause ends. |
| **Sign-in expired** | The stored sign-in no longer works. On your own account the row shows **Sign in again** beside the status. An account shared with you never shows this status, even when its sign-in expired: only its owner sees it and can sign in again. |
| **Active · no Fable** | Claude Code only. The next request goes here, but this seat cannot serve Fable models. |
| **Active · Fable** | Claude Code only. Fable requests go here, because the account above it cannot serve them. |
| **Needs attention** | The server sent a status this version of the page does not know. |

- **On credits**, a badge beside the status. A Codex account shows it while it takes requests only because it spends paid credits.
- **Couldn't read usage**, a short note beside the status when the last usage check failed. Hover it to see the provider's error. The bars keep the last values that were read. The note is not shown on accounts shared with you, on **Sign-in expired** rows, or for check errors that leave requests working, such as a rate limit on the usage check itself.
- **Usage bars**, one per usage window the provider reports, such as **5h** and **Week**, each with the percentage used and when it resets. A verified Claude Ultra account has no 5-hour limit, so it shows only its weekly bars. Antigravity shows **Gemini 5h** and **Gemini Week**, then **Argon Week** on Google AI Pro, where Gemini 4 Argon has its own weekly quota, then **Other 5h** and **Other Week**. Each of these bars covers one group of models. A full one blocks only the models in its group, so the account's status does not change. A bar without a group name, such as **Week**, covers every model: a full one turns the row **Limit reached**. A new account shows **Usage will appear once this account is used**. On a **Sign-in expired** account the bars are dimmed: they show the last usage that was read.
- **Credits**, a row under the bars when Codex or Antigravity reports a credit balance, such as `1,500 left` or `Unlimited`. A Codex account that has credits without a balance shows `Available`. A balance has no total, so it has no bar.

### Custom endpoints

One row per custom endpoint: its name, its format (**OpenAI** or **Anthropic**), the **Model prefix** its ids use (such as `mygw/*`), the base URL (**Endpoint**), a masked key (**Key**) and a status, **Active** or **Paused**. An OpenAI-format endpoint with a token-count URL also shows that URL as **Token count**. A custom endpoint has no usage windows to show.

### CLI providers

One row per local model server registered from the CLI: its name and prefix, format, **Connected** or **Offline**, how many models it last reported, and the device it was registered from. There is no add button here. You register a server from the terminal; see [Local models](/cli/local-models).

### Edit mode

Rows show identity and usage only. The one exception is **Sign in again** on your own accounts whose sign-in expired. To change anything else, choose the pencil in a section's header (**Edit Claude Code**, **Edit Custom endpoints**, **Edit CLI providers**, and so on). The pencil appears once the section has at least one row. It reveals the actions on every row of that section. Choose it again, now a check, to close them. The plus button is hidden while edit mode is open.

In edit mode a subscription card shows a settings block between its header and its rows. It holds **Strategy**. **Ordered** is the only option: priority order, first usable account. Each of your own accounts also shows its own checkboxes at the bottom of its row, described in [Request edits](#request-edits). Custom endpoints and CLI providers have their own checkboxes too; [Request edits](#request-edits) lists where each one is.

### Request edits

Request edits are up to two checkboxes per account that change what Kano Proxy sends to it. Each account has its own, so two Claude Code accounts can be set differently. Both are cleared by default. A box is saved as soon as you select or clear it.

**Remove fast mode** sends requests at standard speed. Claude bills fast mode to usage credits, even when plan limits are left. Codex fast mode uses up plan limits faster.

**Remove the Claude Code line** removes Claude Code's identity line, `You are Claude Code, Anthropic's official CLI for Claude.`, from system prompts. It also removes the line's two Claude Agent SDK versions: `You are Claude Code, Anthropic's official CLI for Claude, running within the Claude Agent SDK.` and `You are a Claude agent, built on Anthropic's Claude Agent SDK.` Claude Code puts this line in every request. When it calls another provider's model through Kano Proxy, the line reaches that provider too, so every account has this checkbox. On Claude Code accounts, Kano Proxy also stops adding the line itself. Without it, Claude may refuse requests or bill them to usage credits.

Where the boxes are, in edit mode:

- **Claude Code** and **Codex** accounts: both boxes, under **Request edits for this account** at the bottom of each of your own rows.
- **Grok** and **Antigravity** accounts: **Remove the Claude Code line** only, in the same place. Kano Proxy never sends fast mode to these providers.
- Each custom endpoint: both boxes, on its row under its details.
- Each CLI provider: both boxes, in its own group under the **CLI providers** table.

A selected box shows a tag: **Fast mode removed** or **Claude Code line removed**. A subscription account shows it on its row, after its status and plan. A custom endpoint shows it beside its status and format, under its name. A CLI provider shows it beside its prefix and format. The tag stays visible with edit mode open or closed.

Each account applies only its own boxes. When a request moves on to the next account in the pool, or a [group](/app/groups) falls back from one provider to another, the next account applies its own boxes. With a box selected, a provider may answer differently or refuse the request.

An account shared with you follows its owner's boxes only. Its row shows the owner's tags but no checkboxes: only the owner can change them, on their own Providers page.

## Common tasks

### Connect a subscription account

1. Choose the plus button (**Add account**) in the provider's card. The dialog opens with a short warning: using a subscription account outside the provider's own app may break its terms. Its **Risks and terms** link opens [Risks and terms](/guide/risks).
2. Choose **Continue**. The provider's own sign-in page opens in a new tab. If your browser blocked it, choose **Open sign-in page**.
3. Finish the sign-in the way the dialog describes for that provider:
   - **Claude Code.** Approve access, copy the authorization code you are given, paste it into the dialog, then choose **Finish**.
   - **Codex** and **Grok.** Enter the code the dialog shows on the page that opened, then approve. The dialog notices on its own after a few seconds; **I've approved it** checks right away.
   - **Antigravity.** Approve access. Google then sends you to a `localhost` address that does not load. That is expected. Copy that address from the browser's address bar, paste it into the dialog, then choose **Finish**.

The new account goes to the top of the pool and becomes **Primary**. You can connect several accounts of the same provider.

If you already hold tokens for an account, **Enter tokens manually** takes an access token, an optional refresh token and an optional label instead.

### Fix an expired sign-in

1. On the row that reads **Sign-in expired**, choose **Sign in again**. You do not need edit mode.
2. Sign in with the same account, as in [Connect a subscription account](#connect-a-subscription-account). **Enter tokens manually** is not offered here.

The account keeps its place in the order, its name and its group settings. Kano Proxy compares the two when both carry an email or an account id. A different account then fails, and the dialog names both accounts. When they cannot be compared, the new sign-in replaces the old one, so make sure it is the same account.

### Change the order of a pool

1. Choose the pencil in the provider's card.
2. Use **Move up** and **Move down** on a row. The order is saved at once.

The top row is **Primary** and is tried first. Order matters only between usable accounts: a limited or paused account is skipped wherever it sits.

### Resume a paused account

1. Choose the pencil in the provider's card.
2. Choose the play button (**Resume**) on the paused row.

The account takes requests again right away. If the provider rejects it again, it is paused again. Resume does not appear for **Limit reached**: a full usage window is a limit set by the provider, and only its reset clears it. It does not appear for **Sign-in expired** either: that needs **Sign in again**.

Resume is offered only on your own accounts. A paused account shared with you is resumed, or signed in again, by its owner.

### Remove fast mode or the Claude Code line

For a subscription account:

1. Choose the pencil in the provider's card.
2. At the bottom of the account's row, under **Request edits for this account**, select **Remove fast mode** or **Remove the Claude Code line**. It is saved at once, and the matching tag appears on the row.
3. Choose the check to close edit mode.

Each account is set on its own row, so repeat step 2 for every account you want to change. For a custom endpoint, choose the pencil in the **Custom endpoints** card, then select the box on the endpoint's row. For a CLI provider, choose the pencil in the **CLI providers** card, then select the box in that provider's group under the table. Clear a box the same way to undo it.

### Let a Codex account use credits after its limits

1. Choose the pencil in the **Codex** card.
2. Select **Use credits after limits** under the account's usage bars.

The box appears only on your own accounts that report credits. Once selected, it stays on the row even after the credits run out, so you can always clear it.

Credits are spent only after the plan's limits run out. While the account runs on credits, it reads **Active** or **Standby** instead of **Limit reached**, with **On credits** beside the status. Clear the box to stop.

### Rename or remove an account

In edit mode, the pencil on a row (**Rename**) sets a display name. Leave the name blank to go back to the provider's own. Renaming changes nothing about routing.

The trash button (**Remove**) asks for confirmation, then deletes the account. Requests stop using it immediately. Adding it back means signing in again.

Rename and Remove appear only on your own accounts.

### Add a custom endpoint

1. In the **Custom endpoints** card, choose the plus button (**Add endpoint**).
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

- The play button (**Resume**), shown only on a **Paused** endpoint. The endpoint takes requests again right away.
- **Test**, which sends a probe to the endpoint and shows the result on the row.
- The pencil (**Edit**), which opens the same form. Leave **API key** blank to keep the stored key.
- The trash button (**Remove**), which deletes the endpoint and its key after you confirm. Requests to `<prefix>/…` stop resolving.
- **Move up** and **Move down**, which change only the display order on this page. They do not affect routing.
- **Remove fast mode** and **Remove the Claude Code line**, described in [Request edits](#request-edits).

Changing the **Model prefix** of an existing endpoint opens a confirmation first. It lists the group targets that will be rewritten to the new prefix. Clients that still call the old prefix fail with an invalid model error until you update them.

### Rename or remove a CLI provider

In edit mode, the pencil on a CLI provider's row (**Rename**) sets its display name. The trash button (**Remove**) asks for confirmation, then removes the provider. Requests to `<prefix>/…` stop resolving immediately.

## Related pages

- [Models](/app/models): every model id the connected providers offer.
- [Groups](/app/groups): your own model names with a failover order across providers and accounts.
- [How requests are routed](/reference/routing): how a request picks an account, and when it moves on.
- [Local models](/cli/local-models): connect a model server on your own machine.
- [Claude Code](/agents/claude-code): point Claude Code at the proxy.
