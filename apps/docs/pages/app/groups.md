---
title: Groups
description: What the Groups page in Kano Proxy does, model groups as private endpoints with your own model names, failover order across providers and accounts, and how to create and edit them.
---

# Groups

Groups is where you build model groups. A model group is an endpoint of your own: you choose the model names a client sends, and for each name an ordered list of real models to run.

## What a group is for

A group has its own base URLs:

| Protocol | Group base URL |
|----------|----------------|
| OpenAI-compatible | `https://<your-domain>/g/<group-slug>/openai/v1` |
| Anthropic Messages | `https://<your-domain>/g/<group-slug>/anthropic` |

A client pointed at those URLs uses the same API key as anywhere else. It can only call the model names the group defines. Each name maps to one or more **targets**: a `provider/model` id, tied to one account. The proxy tries the targets in order and uses the first one that can take the request.

Two common uses:

- **A tool with fixed model names.** Some tools only send names like `claude-opus-5` or `gpt-4o`, without a `provider/` prefix. Define those exact names in a group and point the tool at the group's URL. The names then run whatever you mapped them to.
- **Failover across accounts and providers.** List several targets under one name. When the first is limited, paused or failing, the request moves to the next one, even within the same request. A pool on [Providers](/app/providers) only fails over between accounts of one provider. A group can go from a Claude Code account to a Codex account to a custom endpoint.

Two groups may use the same model name for different things, because each group is its own endpoint. On the shared base URLs, `/openai/v1` and `/anthropic`, group names do not resolve.

## What is on the page

### Group list

The left column lists your groups. Each row shows the group's name, its slug and how many models it has. A warning dot means at least one of its targets cannot take requests right now. Use the arrow keys to move between groups.

On a narrow screen only the list shows. Tap a group to open it, and **All groups** to go back.

### Group detail

The right side shows the selected group:

- **The name**, when it was last updated, and **Edit**.
- **Two endpoint fields**, **OpenAI** and **Anthropic**, each with the full base URL for this site and a copy button. This URL is what goes in a client's base URL setting.
- **The models table.** One row per model name.
  - **Model** is the name a client sends as `model`. Click it to copy.
  - **Route · failover order** shows the targets left to right, tried in that order. Each target shows its `provider/model` id and, under it, the account it uses.
  - A target tied to an account uses only that account, and is skipped when that account cannot take the request. A target marked **Any account** uses the provider's whole pool in its own order. Targets you add on this page are always tied to an account. Targets the [CLI](/cli/agents) creates use **Any account**.

The table also shows where the next request would go:

- The target drawn in the accent color, marked **Current**, is the one the next request for that name would use.
- A struck-through target cannot take requests right now, with the reason beside it, for example **Limit reached · resumes in 2 hours** or **Paused · resumes in 4 minutes**.
- **Account removed** means the target's account was deleted. It is skipped until you pick another account.
- **This model can't be reached anymore** means its provider is gone, for example a deleted custom endpoint.

This view reads the same stored state the proxy uses to route, so it matches what the next request would do.

## Common tasks

### Create a group

1. Choose **Create group**.
2. In the first column, enter a **Name**. It is a label for you; clients never send it.
3. Enter the **Endpoint slug**: lowercase letters, digits and hyphens, 2 to 32 characters. The two base URLs it produces appear below it.
4. In the third column, under **Group models**, type a **New model name** and choose **Add model**. Use the exact name your client sends. Names can contain `/` but no spaces.
5. In the middle column, **Pick targets**:
   1. Choose a provider tab across the top.
   2. Choose an account in the list on the left. Its models appear.
   3. Click a model. It is added as a target of the selected model name, tied to that account.
6. Repeat the last step to add fallback targets. To add the same model on a second account, pick the other account and click the same model.
7. Use **Move up** and **Move down** on a target to change its position. The first target is tried first.
8. Choose **Create group**.

A group needs at least one model, and every model needs at least one target. Limits: 50 groups, 20 models per group, 20 targets per model.

**Strategy** in the first column has one option, **Ordered**: targets are tried in list order.

### Point a client at a group

1. Select the group.
2. Copy the **OpenAI** or **Anthropic** base URL, whichever your client speaks.
3. Set it as the client's base URL, with an API key from [Access](/app/access).
4. Set the client's model to one of the group's model names.

The [CLI](/cli/agents) creates groups this way for Claude Code and Codex CLI, named `claude-code` and `codex-cli`.

### Change targets or names

1. Select the group and choose **Edit**.
2. Click a model name in the third column to select it. New picks go to the selected name.
3. Rename a model by editing its name field, remove one with **Remove model**, and reorder or remove targets on each target row.
4. Choose **Save changes**.

Changing the slug moves the group's URLs immediately. The old URLs stop working, so update every client that uses them.

### Delete a group

1. Select the group and choose **Edit**.
2. Choose **Delete group**, then **Delete** to confirm.

The group's endpoint stops working immediately. Your accounts, keys and logs are not affected.

## What a client sees

Responses echo the model name the client sent, not the target that answered. A non-streaming response also names the real target in the `x-kano-upstream-provider` and `x-kano-upstream-model` headers. [Logs](/app/logs) records both: the target that answered, and the group and model name the request was sent to.

Requesting a name the group does not define returns `400 invalid_model`. A slug that is not one of your groups returns `404`.

## Related pages

- [Providers](/app/providers): connect the accounts that targets use.
- [Models](/app/models): every `provider/model` id you can use as a target.
- [How requests are routed](/reference/routing): when a target or account is skipped, and when a request gives up.
- [Endpoints and model ids](/reference/endpoints): the shared base URLs and the id format.
