---
title: Models
description: What the Models page shows in Kano Proxy, every provider/model id you can call right now grouped by provider, and how to search and copy ids.
---

# Models

Models lists every model id you can call right now, grouped by the provider it comes from. Copy an id here and put it in your client's `model` field.

The list is live. It comes from the accounts and endpoints you connected on [Providers](/app/providers), so it changes when you connect or remove one.

## What is on the page

### Header

- **Search models** filters every group by model id and name as you type.
- The refresh button asks each provider for its list again.
- The tabs below the title narrow the page to one provider. **All models** shows every group.

### One card per provider

Each provider gets its own card, in this order:

1. The built-in providers: Claude Code, Codex, Grok, Antigravity.
2. Each custom endpoint you added.
3. Each local model server connected through the [CLI](/cli/local-models).

The card title is the provider's name. A custom endpoint or local model also shows its format, OpenAI or Anthropic, and the prefix its ids carry, such as `mygw/*`. The number on the right is how many models the card lists.

Each row has the model id on the left and its name on the right. Click a row, or its copy button, to copy the id.

Every id has the form `provider/model`, for example `claude-code/claude-opus-5` (an example only; read your own list). The same id works on both the OpenAI and the Anthropic base URL. [Endpoints and model ids](/reference/endpoints) explains the format.

Model groups do not appear here. A group has its own endpoint and its own model names, listed on [Groups](/app/groups).

### Empty and error states

A card shows a message instead of rows when it has nothing to list:

- **Connect an account to load these models.** No usable account is connected for that provider.
- **This provider doesn't publish a model list.** Codex has no public list. Use the model id your ChatGPT plan offers, prefixed with `codex/`.
- **Add model ids to this endpoint, or let it load them automatically.** A custom endpoint with no models. Edit it on Providers.

If a provider's list fails to load, a warning with its error appears inside that provider's card. The other cards are not affected.

**No models yet** across the whole page means nothing is connected. Choose **Connect a provider** to go to Providers.

## Common tasks

### Copy a model id for a client

1. Type part of the name in **Search models**, for example `opus`.
2. Click the row. The copy button turns into a check.
3. Paste the id into the client's `model` setting.

### Make a new model show up

1. Choose refresh. The list is otherwise kept for up to an hour.
2. If it is still missing, check on Providers that the account is connected and not in an error state. For a custom endpoint, add the id to the endpoint's model list.

### Read the list from a script

Clients get the same list from the API, with an API key:

```bash
curl https://<your-domain>/openai/v1/models -H "Authorization: Bearer <your-api-key>"
```

## Related pages

- [Providers](/app/providers): connect the accounts these models come from.
- [Groups](/app/groups): give models your own names and a failover order.
- [Endpoints and model ids](/reference/endpoints): base URLs and the id format.
