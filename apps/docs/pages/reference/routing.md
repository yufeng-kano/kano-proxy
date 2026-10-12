---
title: How requests are routed
description: How Kano Proxy picks a provider, an account and a group target for each request, when it skips or pauses an account, how failover works, and when it gives up.
---

# How requests are routed

Every request ends up on one account of one provider. The model id decides the provider, the order you set decides the account, and an account that cannot take the request is skipped. If every option is used up, the request fails with an error that says so.

## 1. The model id picks the provider

On the shared base URLs, `/openai/v1` and `/anthropic`, the model id is `provider/model`. It is split at the first `/`:

- The part before it is a built-in provider (`claude-code`, `codex`, `grok`, `antigravity`), or the prefix of one of your custom endpoints or CLI providers.
- The rest is passed to that provider as its model id.

An id without a `/`, or with a prefix you do not have, is rejected with `400 invalid_model`.

On a group endpoint, `/g/<group-slug>/...`, the model must exactly match one of the group's model names. That name then expands to its list of targets, in order. See [Groups](/app/groups).

## 2. The order decides who is tried first

The proxy turns the request into one ordered list of candidates, each an account on a provider:

- **A `provider/model` id.** Every account in that provider's pool, in the order shown on [Providers](/app/providers). **Primary** comes first.
- **A group model name.** Its targets in list order. A target tied to an account adds just that account. A target marked **Any account** adds the provider's whole pool, in pool order.

The only strategy today is **Ordered**: the first usable candidate gets the request. The proxy does not spread load across accounts. Staying on one account keeps a conversation on the same upstream, where its prompt cache is warm.


## 3. Accounts that cannot take the request are skipped

Before anything is sent, a candidate is skipped when:

- **It is paused.** A recent request to it failed in a way that pauses the account (table below). It stays paused until the time shown on Providers, or until you choose **Resume**.
- **A usage window is full.** Any window the provider reports, such as **5h** or **Week**, at 100%. The account shows **Limit reached** and comes back when that window resets. Resume does not override this.
- **It cannot serve the model.** Today one rule exists: a standard Claude Team seat cannot serve Fable models, so `claude-fable-*` requests skip it. Its other requests are unaffected.
- **Its account or provider no longer exists**, for example a deleted custom endpoint in a group.

The skip decisions use what the proxy already knows about each account. They never wait for an extra call to the provider. Usage is refreshed in the background while traffic flows, at most once every 2 minutes per account.

## 4. Failover during a request

When a provider answers with an error, the proxy decides whether to try the next candidate:

| Provider answers | What happens to the account | Next candidate |
|------------------|-----------------------------|----------------|
| `401`, `403` (sign-in or permission), `402` (billing) | Paused for 5 minutes | Yes |
| `429` (rate limit) | Paused until the provider's reset time when it can be read from the response or the usage windows, otherwise 5 minutes, never more than 7 days | Yes |
| `520`, `522`, `524` (the provider's own edge timed out) | Skipped for this request only. The third time within 10 minutes pauses it for 30 seconds | Yes |
| `529` (provider overloaded) | Never paused. Retried once on the same account after about a second | Yes, if the retry also fails. The last candidate's `529` is returned as is |
| No response headers within the timeout (3 minutes by default) | Skipped for this request only | Yes |
| Any other error | Nothing | No. The error is returned to the client |

For a [local model](/cli/local-models), a computer that is offline pauses that provider for 60 seconds. Other connection problems between the proxy and the CLI skip it for that request only.

A request makes at most 8 attempts. Once a provider has started streaming its answer, the request stays on that account: switching mid-answer would mix two answers.

[Logs](/app/logs) records one row per request, with the account that answered, or the last one tried when the request failed.

## 5. When the request gives up

| Situation | Response |
|-----------|----------|
| The prefix is not a provider you have, or the group has no such model | `400 invalid_model` |
| No account is connected for the provider, or none can serve this model | `400 no_upstream_account` |
| Accounts exist, but all are paused or limited, or every one tried failed with a status from the table above | `503 upstream_unavailable` |

The `503` carries a `Retry-After` header with the seconds until the earliest account comes back, at most 60. Clients that honor it retry soon instead of waiting for a weekly reset. With `stream: true` the same errors arrive as an error event inside the stream, because the response has already started. [Errors](/reference/errors) lists every error code.

## Check where requests go

- [Providers](/app/providers): the account marked **Active** is where the next request for that provider goes. Paused and limited accounts show when they come back.
- [Groups](/app/groups): the target marked **Current** is where the next request for that model name goes.
- [Logs](/app/logs): the provider, model and account behind each request.

## Related pages

- [Providers](/app/providers): connect accounts, change their order, resume a paused account.
- [Groups](/app/groups): ordered targets across providers and accounts.
- [Endpoints and model ids](/reference/endpoints): base URLs and the id format.
- [Errors](/reference/errors): every error code and what to do about it.
