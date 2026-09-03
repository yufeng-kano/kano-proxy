/**
 * English message catalog — the reference locale.
 *
 * This is the *only* place user-facing copy lives. A string typed directly
 * into a template is a bug: it cannot be translated, and it escapes the review
 * this file gets.
 *
 * Voice rules for anything added here (see docs/i18n.md):
 *
 * - Speak to what the user gets, never to how the app works. No cache TTLs,
 *   poll intervals, "last updated", storage layers, or refresh mechanics —
 *   those are implementation facts, and the product surface is not their
 *   documentation.
 * - Errors say what failed and what to do, not which layer threw.
 * - Sentence case for everything except proper nouns; no trailing period on
 *   labels, buttons, or table headers; periods only in full sentences.
 * - `{placeholder}` interpolation; `…_one` / `…_other` sibling keys for
 *   anything counted, chosen by `Intl.PluralRules`.
 */

export const en = {
  // --- App chrome ---------------------------------------------------------
  "app.skipToContent": "Skip to content",
  "app.loading": "Loading",
  "app.closeMenu": "Close menu",
  "app.openMenu": "Open menu",
  "app.primaryNav": "Main",
  "app.signOut": "Sign out",
  "app.updateAvailable": "Update available",

  // --- Navigation ---------------------------------------------------------
  "nav.overview": "Overview",
  "nav.logs": "Logs",
  "nav.providers": "Providers",
  "nav.models": "Models",
  "nav.groups": "Groups",
  "nav.keys": "API keys",
  "nav.changelog": "Changelog",

  // --- Shared actions -----------------------------------------------------
  "action.refresh": "Refresh",
  "action.retry": "Try again",
  "action.cancel": "Cancel",
  "action.close": "Close",
  "action.back": "Back",
  "action.copy": "Copy",
  "action.copied": "Copied",
  "action.dismiss": "Dismiss",
  // The row actions that keep a visible word: the one that destroys something
  // (Remove) and the one no glyph names on sight (Test). Every other row action
  // is a glyph whose words live in its accessible name — those names carry the
  // row's subject too (`providers.account.*`, `custom.*Endpoint`), since the
  // same action repeats down a list, and where a visible word exists the name
  // must contain it verbatim (WCAG 2.5.3).
  "action.remove": "Remove",
  "action.test": "Test",
  // Names the trailing edit column of a table, rendered visually hidden: a
  // blank `<th>` is an unnamed column to a screen reader.
  "action.edit": "Edit",
  "action.search": "Search",
  "action.done": "Done",

  // --- Shared states ------------------------------------------------------
  "state.copyFailed": "Couldn't copy to the clipboard",
  "state.never": "Never",

  // --- Status -------------------------------------------------------------
  "status.active": "Active",
  "status.activeNoFable": "Active · no Fable",
  "status.activeFable": "Active · Fable",
  "status.standby": "Standby",
  "status.limited": "Limit reached",
  "status.benched": "Paused",
  "status.unusable": "Needs attention",

  // --- Overview (dashboard) ----------------------------------------------
  "overview.title": "Overview",
  "overview.range.day": "Day",
  "overview.range.week": "Week",
  "overview.range.month": "Month",
  "overview.range.24h": "24 hours",
  "overview.range.7d": "7 days",
  "overview.range.30d": "30 days",
  "overview.range.label": "Time range",
  "overview.range.short.day": "Day",
  "overview.range.short.week": "Week",
  "overview.range.short.month": "Month",
  "overview.range.short.24h": "24h",
  "overview.range.short.7d": "7d",
  "overview.range.short.30d": "30d",
  "overview.range.title.day": "Day (hourly view — click to pick date)",
  "overview.range.title.week": "Week (daily view — click to pick week)",
  "overview.range.title.month": "Month (daily view — click to pick month)",
  "overview.calendar.today": "Today",
  "overview.calendar.thisWeek": "This week",
  "overview.calendar.thisMonth": "This month",
  "overview.calendar.prevMonth": "Previous month",
  "overview.calendar.nextMonth": "Next month",
  "overview.calendar.prevYear": "Previous year",
  "overview.calendar.nextYear": "Next year",
  // Accessible names for the calendar cells: the visible label is a bare
  // number or a three-letter month, which says nothing on its own.
  "overview.calendar.weekOf": "Week of {start} to {end}",
  "overview.empty.title": "No requests yet",
  "overview.empty.body":
    "Once your clients start calling, usage and cache savings show up here.",
  "overview.empty.action": "Get your API key",
  "overview.metric.spend": "Spend",
  "overview.metric.requests": "Requests",
  "overview.metric.tokens": "Tokens",
  "overview.stat.errors": "Errors",
  "overview.stat.latency": "Avg latency",
  "overview.card.expand": "Expand {metric}",
  "overview.card.topModels": "Top models by {metric}",
  "overview.card.others": "Others",
  "overview.card.noActivity": "No activity in this range",
  "overview.spend.estimated": "Estimated from list prices",
  "overview.spend.coverage": "Estimated · covers {known} of {total} requests",
  "overview.spend.none": "No priced requests in this range",
  "overview.activity.label": "Activity",
  "overview.tab.tokens": "Tokens",
  "overview.tab.requests": "Requests",
  "overview.tab.cache": "Caching",
  "overview.tab.models": "By model",
  "overview.cache.cached": "Cached",
  "overview.cache.uncached": "Uncached",
  "overview.chart.emptyBucket": "No activity",
  "overview.chart.total": "Total",
  "overview.chart.bucketAria": "{when}: {detail}",
  "overview.chart.bucketNoData": "no activity",
  "overview.detail.model": "Model",
  "overview.detail.min": "Min",
  "overview.detail.max": "Max",
  "overview.detail.avg": "Avg",
  "overview.detail.sum": "Sum",
  "overview.detail.caption": "{metric} per model over the range",
  "overview.models.model": "Model",
  "overview.models.requests": "Requests",
  "overview.models.errors": "Errors",
  "overview.models.input": "Input",
  "overview.models.cached": "Cached",
  "overview.models.cacheWrite": "Cache write",
  "overview.models.output": "Output",
  "overview.models.cacheRate": "Cache rate",
  "overview.models.spend": "Spend",
  "overview.models.coverage": "on {known} of {total}",
  "overview.models.empty": "No model activity in this range",
  "overview.error.load": "Couldn't load your usage",

  // --- Logs ---------------------------------------------------------------
  "logs.title": "Logs",
  // Filters. "Errors" is the whole second option, so the group's name has to
  // say what is being narrowed rather than repeating one of its options.
  "logs.filter.provider": "Provider",
  "logs.filter.allProviders": "All providers",
  "logs.filter.show": "Show",
  "logs.filter.showAll": "All",
  "logs.filter.showErrors": "Errors",
  "logs.column.time": "Time",
  "logs.column.model": "Model",
  "logs.column.account": "Account",
  "logs.column.apiKey": "API key",
  "logs.column.type": "Type",
  "logs.column.status": "Status",
  "logs.column.input": "Input",
  "logs.column.cacheRead": "Cache read",
  "logs.column.cacheWrite": "Cache write",
  "logs.column.output": "Output",
  "logs.column.cost": "Cost",
  "logs.column.latency": "Latency",
  // "via" names the alias the client actually sent, so a group's traffic reads
  // apart from a direct call to the same model; "Account removed" and
  // "Key removed" are records deleted since the request ran — which happened
  // all the same, so the row stays and says which part of it is gone.
  "logs.via": "via {group}",
  "logs.accountRemoved": "Account removed",
  "logs.keyRemoved": "Key removed",
  "logs.type.oauth": "OAuth",
  "logs.type.api": "API",
  // The row's own control: the visible text is the timestamp, so the name it
  // is given has to contain it verbatim (WCAG 2.5.3).
  "logs.openDetail": "Details for {time}",
  "logs.loadMore": "Load more",
  "logs.detail.title": "Request detail",
  "logs.detail.id": "Request id",
  "logs.detail.provider": "Provider",
  "logs.detail.alias": "Alias",
  "logs.detail.account": "Account",
  "logs.detail.apiKey": "API key",
  "logs.detail.keyRemoved": "Key removed",
  "logs.detail.upstreamStatus": "Upstream status",
  "logs.detail.error": "Error",
  "logs.empty.title": "No requests yet",
  "logs.empty.body": "Once your clients start calling, every request shows up here.",
  "logs.noResults.title": "No matching requests",
  "logs.noResults.body": "Nothing matches the filters you picked.",
  "logs.error.load": "Couldn't load your requests",
  "logs.error.loadMore": "Couldn't load more requests",

  // --- Providers ----------------------------------------------------------
  "providers.title": "Providers",
  "providers.all": "All",
  "providers.group.custom": "Custom",
  "providers.addAccount": "Add account",
  // One gate per section, so its name is the section's — not a row's.
  "providers.section.edit": "Edit {section}",
  "providers.section.doneEditing": "Done editing {section}",
  // Accessible names for the row's actions. Resume, promote and rename are
  // glyphs, so these are the only words they have — name and tooltip both.
  // Remove is the one with visible text (`action.remove`), which its name
  // repeats verbatim.
  "providers.account.resume": "Resume {name}",
  "providers.account.promote": "Make {name} primary",
  // The badge on the account requests route through first — the same word the
  // promote name ends on, so the control and the badge state one fact.
  "providers.account.primary": "Primary",
  "providers.account.rename": "Rename {name}",
  "providers.account.remove": "Remove {name}",
  "providers.account.removeConfirm":
    "Remove this account? Requests will stop routing through it.",
  "providers.account.noUsage": "Usage will appear once this account is used",
  // Antigravity reports a credit balance beside its quota windows; it has no
  // total, so it is a line and not a bar — docs/providers.md § Antigravity.
  "providers.account.credits": "{credits} credits left",
  "providers.account.plan": "{plan} plan",
  "providers.rename.title": "Rename account",
  "providers.rename.label": "Name",
  "providers.rename.placeholder": "Work account",
  "providers.rename.hint": "Leave blank to use {identity}",
  "providers.rename.tooLong": "Use 64 characters or fewer",
  "providers.rename.save": "Save",
  "providers.usage.resets": "Resets {when}",
  "providers.empty.title": "No {provider} account yet",
  "providers.empty.body": "Connect one to start routing {provider} models.",
  "providers.error.load": "Couldn't load {provider} accounts",
  "providers.error.resume": "Couldn't resume the account",
  "providers.error.promote": "Couldn't change the primary account",
  "providers.error.remove": "Couldn't remove the account",
  "providers.error.rename": "Couldn't rename the account",
  "providers.error.strategy": "Couldn't change the routing strategy",

  // --- Routing strategy ---------------------------------------------------
  // How a pool or a group decides what a request runs on. One option ships
  // today and the control still says so out loud: knowing there is a routing
  // policy at all is the point, not choosing between two. The two description
  // lines answer the same question on the two surfaces — a pool orders the
  // accounts bound to it, a group orders the targets the user listed.
  "strategy.label": "Strategy",
  "strategy.ordered": "Ordered",
  "strategy.pool.ordered": "Priority order, first usable account",
  "strategy.group.ordered": "Targets tried in list order",

  // --- Provider descriptions (product copy, not mechanism) ----------------
  "provider.claude-code.name": "Claude Code",
  "provider.claude-code.blurb": "Your Anthropic subscription",
  "provider.codex.name": "Codex",
  "provider.codex.blurb": "Your ChatGPT subscription",
  "provider.grok.name": "Grok",
  "provider.grok.blurb": "Your xAI subscription",
  "provider.antigravity.name": "Antigravity",
  "provider.antigravity.blurb": "Your Google AI subscription",
  "provider.custom.name": "Custom endpoints",

  // --- Add account dialog -------------------------------------------------
  "addAccount.title": "Add {provider} account",
  "addAccount.intro": "Sign in with your {provider} account to add it to the pool.",
  "addAccount.start": "Continue",
  "addAccount.manual": "Enter tokens manually",
  "addAccount.openAuth": "Open sign-in page",
  "addAccount.claude.step1": "Approve access on the page that opened.",
  "addAccount.claude.step2": "Copy the code you're given and paste it below.",
  "addAccount.claude.label": "Authorization code",
  // Antigravity's sign-in ends on a localhost address nothing is serving, so
  // the browser shows an error page. That is expected, and the copy has to say
  // so before the user reads it as a failure and starts over.
  "addAccount.antigravity.step1": "Approve access on the page that opened.",
  "addAccount.antigravity.step2":
    "Google sends you to a localhost address that won't load. That's expected.",
  "addAccount.antigravity.step3": "Copy that address from the browser and paste it below.",
  "addAccount.antigravity.label": "Callback address",
  // Shared by every device-code provider (Codex, Grok) — the copy describes
  // the flow, not the brand, so it must stay provider-neutral.
  "addAccount.device.intro": "Enter this code on the page that opened.",
  "addAccount.device.waiting": "Waiting for you to approve…",
  "addAccount.device.check": "I've approved it",
  "addAccount.complete": "Finish",
  "addAccount.manual.title": "Enter tokens manually",
  "addAccount.manual.intro": "For accounts you already hold credentials for.",
  "addAccount.manual.accessToken": "Access token",
  "addAccount.manual.refreshToken": "Refresh token",
  "addAccount.manual.label": "Label",
  "addAccount.manual.optional": "Optional",
  "addAccount.manual.submit": "Add account",
  "addAccount.error.start": "Couldn't start sign-in",
  "addAccount.error.complete": "Couldn't finish sign-in",
  "addAccount.error.token": "Enter an access token",

  // --- Custom endpoints ---------------------------------------------------
  "custom.add": "Add endpoint",
  // Accessible names for the row's actions. Resume and Edit are glyphs, so
  // these are their only words; Test and Remove keep visible text, which their
  // names repeat verbatim.
  "custom.resumeEndpoint": "Resume {name}",
  "custom.testEndpoint": "Test {name}",
  "custom.editEndpoint": "Edit {name}",
  "custom.removeEndpoint": "Remove {name}",
  // Reordering is display only — the copy must not suggest it changes routing
  // or which key a request uses.
  "custom.reorder.moveUp": "Move {name} up",
  "custom.reorder.moveDown": "Move {name} down",
  "custom.reorder.moving": "Moving",
  "custom.reorder.moved": "{name} is now {position} of {total}",
  "custom.error.reorder": "Couldn't save the new order",
  "custom.empty.title": "No custom endpoints",
  "custom.empty.body":
    "Point at any OpenAI- or Anthropic-compatible API and call it like every other model.",
  "custom.field.modelId": "Model prefix",
  "custom.field.endpoint": "Endpoint",
  "custom.field.key": "Key",
  "custom.field.countTokens": "Token count",
  "custom.removeConfirm": "Remove \"{name}\"? Its stored key is deleted too.",
  "custom.test.ok": "Connected",
  "custom.test.okModels_one": "Connected · {count} model",
  "custom.test.okModels_other": "Connected · {count} models",
  "custom.test.failed": "Couldn't connect",
  "custom.test.sample": "For example: {models}",
  "custom.dialog.addTitle": "Add endpoint",
  "custom.dialog.editTitle": "Edit endpoint",
  "custom.dialog.format": "API format",
  "custom.dialog.formatOpenAI": "OpenAI",
  "custom.dialog.formatAnthropic": "Anthropic",
  "custom.dialog.formatLocked": "Can't be changed later",
  "custom.dialog.name": "Name",
  "custom.dialog.namePlaceholder": "My endpoint",
  "custom.dialog.slug": "Model prefix",
  "custom.dialog.slugPlaceholder": "my-endpoint",
  "custom.dialog.slugHint": "Call models as {example}",
  "custom.dialog.slugLocked": "Can't be changed later",
  "custom.dialog.baseUrl": "Base URL",
  "custom.dialog.baseUrlHint": "Requests go to {url}",
  "custom.dialog.apiKey": "API key",
  "custom.dialog.apiKeyPlaceholderNew": "sk-…",
  "custom.dialog.apiKeyPlaceholderEdit": "Leave blank to keep the current key",
  "custom.dialog.models": "Models",
  "custom.dialog.modelsAuto": "Load automatically",
  "custom.dialog.modelsAutoHint": "Fetch the model list from this endpoint",
  "custom.dialog.modelsManual": "Enter manually",
  "custom.dialog.modelsManualHint": "List the model ids yourself",
  "custom.dialog.manualModels": "Model ids",
  "custom.dialog.manualModelsHint": "One per line",
  "custom.dialog.countTokensUrl": "Token-count URL",
  "custom.dialog.countTokensUrlOptional": "Optional",
  // A URL, not copy — `example.com` is the reserved documentation domain and
  // the path is the Anthropic wire path, so neither is translated.
  "custom.dialog.countTokensUrlPlaceholder":
    "https://your-gateway.example.com/v1/messages/count_tokens",
  // Says what it buys, and that this one is the whole address: every other URL
  // field here is a base the proxy appends a path to.
  "custom.dialog.countTokensUrlHint":
    "Claude Code asks for a token count, and an OpenAI endpoint has none — point this at an Anthropic-compatible /v1/messages/count_tokens if your gateway has one. It's used exactly as typed, with nothing added to the end. Leave it empty and that request keeps failing as it does today.",
  "custom.dialog.testConnection": "Test connection",
  "custom.dialog.submitAdd": "Add endpoint",
  "custom.dialog.submitEdit": "Save changes",
  "custom.error.name": "Enter a name",
  "custom.error.slug": "Enter a model prefix",
  "custom.error.slugFormat":
    "Use lowercase letters, numbers, and hyphens, starting and ending with a letter or number",
  "custom.error.baseUrl": "Enter a base URL",
  "custom.error.baseUrlInvalid": "Enter a valid URL",
  "custom.error.baseUrlHttps": "The URL must start with https://",
  "custom.error.countTokensUrlInvalid": "Enter a valid token-count URL",
  "custom.error.countTokensUrlHttps": "The token-count URL must start with https://",
  "custom.error.apiKey": "Enter an API key",
  "custom.error.save": "Couldn't save the endpoint",
  "custom.error.resume": "Couldn't resume the endpoint",
  "custom.error.remove": "Couldn't remove the endpoint",
  "custom.error.load": "Couldn't load your endpoints",

  // --- Models -------------------------------------------------------------
  "models.title": "Models",
  "models.searchPlaceholder": "Search models",
  "models.all": "All models",
  "models.count_one": "{count} model",
  "models.count_other": "{count} models",
  "models.column.id": "Model id",
  "models.column.name": "Name",
  "models.copyId": "Copy id",
  "models.empty.title": "No models yet",
  "models.empty.body": "Connect a provider and its models show up here.",
  "models.empty.action": "Connect a provider",
  "models.noResults.title": "No matches",
  "models.noResults.body": "Nothing matches \"{query}\".",
  "models.group.emptyGeneric": "Connect an account to load these models.",
  "models.group.emptyCustom":
    "Add model ids to this endpoint, or let it load them automatically.",
  "models.group.emptyCodex":
    "This provider doesn't publish a model list — use the model id from your ChatGPT plan.",
  // The catalog's fixed `group` section. Every other section is named by the
  // provider it came from; this one is the user's own names, so it gets a label.
  "models.error.load": "Couldn't load models",

  // --- Groups -------------------------------------------------------------
  "groups.title": "Groups",
  "groups.create": "Create group",
  "groups.column.name": "Name",
  "groups.column.endpoint": "Endpoint",
  "groups.column.models": "Models",
  "groups.column.updated": "Updated",
  // Accessible names for the row controls: several rows offer the same two
  // words, so the subject goes in the name. Each contains the visible label
  // verbatim (WCAG 2.5.3) — the copied value itself, and "Edit".
  "groups.copyModel": "Copy {model}",
  "groups.copyUrl": "Copy {url}",
  "groups.editGroup": "Edit {name}",
  "groups.empty.title": "No groups yet",
  "groups.empty.body":
    "A group is an endpoint of your own. Point a client at its base URL, and the model names you define there route to whatever you actually run — one name can try several accounts and providers in order.",
  "groups.delete": "Delete group",
  "groups.deleteConfirm": "Delete this group? Its endpoint stops working immediately.",
  "groups.dialog.editTitle": "Edit group",
  "groups.dialog.save": "Save changes",
  // The three column heads. Nouns, not verbs: they name what each side holds,
  // and they sit on one row, so they are read together.
  "groups.dialog.identityLabel": "Group",
  "groups.dialog.nameLabel": "Name",
  "groups.dialog.namePlaceholder": "Claude Code setup",
  "groups.dialog.nameHint": "A label for this group. Clients never send it.",
  // The endpoint identity: the slug is the URL, so the hint says what it makes
  // and the preview below shows it live.
  "groups.dialog.slugLabel": "Endpoint slug",
  "groups.dialog.slugPlaceholder": "my-tools",
  "groups.dialog.slugHint":
    "Lowercase letters, digits and hyphens. It becomes this group's base URLs:",
  "groups.dialog.slugMoves": "Changing the slug moves these URLs immediately.",
  "groups.dialog.pickerLabel": "Pick targets",
  "groups.dialog.modelsLabel": "Group models",
  "groups.dialog.modelField": "New model name",
  "groups.dialog.modelPlaceholder": "gpt-4o",
  "groups.dialog.addModel": "Add model",
  "groups.dialog.addModelNamed": "Add model {name}",
  "groups.dialog.removeModel": "Remove model {name}",
  "groups.dialog.selectModel": "Edit targets of {name}",
  "groups.dialog.modelNameLabel": "Model name {name}",
  "groups.dialog.modelsHint":
    "Add a model name clients will send on this endpoint, then pick its targets from the catalog on the left.",
  "groups.dialog.targetsEmpty":
    "No targets yet — pick an account on the left, then choose the models it should serve.",
  // The account dimension. "Any account" is the unpinned state the picker no
  // longer creates but older groups still hold — the provider's own pool and
  // priority — and it reads the same on the list page and here.
  "groups.account.any": "Any account",
  "groups.account.missing": "Account removed",
  "groups.account.skipped": "Skipped until you pick another account",
  // --- Where the next request goes. "Current" marks the target the group
  // would dispatch to right now; the rest say why a target is being skipped,
  // in the user's terms ("Paused", not "benched") and with the time it comes
  // back where that is known.
  "groups.route.current": "Current",
  "groups.route.limitUntil": "Limit reached · resumes {when}",
  "groups.route.pausedUntil": "Paused · resumes {when}",
  "groups.route.unavailable": "Unavailable",
  "groups.route.unresolved": "This model can't be reached anymore",
  "groups.route.noAccount": "No account can take this",
  "groups.dialog.accountLabel": "Account for {target}",
  // --- Picker: providers across the top, accounts down the side, models in
  // the middle. Each label names the region it heads, so the three read as one
  // path rather than three unrelated lists.
  "groups.dialog.providersLabel": "Providers",
  "groups.dialog.accountsLabel": "Accounts",
  "groups.dialog.accountsEmpty": "No accounts",
  "groups.dialog.railEmpty.title": "No accounts here",
  "groups.dialog.railEmpty.body": "Bind one on the {page} page and it shows up here.",
  "groups.dialog.pickAccount": "Pick an account to see what it can run.",
  "groups.dialog.searchPlaceholder": "Search models",
  "groups.dialog.modelsEmpty": "This provider lists no models. Add one by id below.",
  "groups.dialog.addModelOn": "Add {model} on {account}",
  "groups.dialog.added": "Added",
  "groups.dialog.add": "Add",
  "groups.dialog.noMatches": "No models match \"{query}\"",
  "groups.dialog.moveUp": "Move {target} up",
  "groups.dialog.moveDown": "Move {target} down",
  "groups.dialog.moved": "{target} is now {position} of {total}",
  "groups.dialog.removeTarget": "Remove {target}",
  "groups.error.load": "Couldn't load your groups",
  "groups.error.name": "Enter a name",
  "groups.error.nameLength": "Use {max} characters or fewer",
  // Alias rules are the id rules — no spaces, no "/" — stated as the fix, not
  // as the constraint that was broken.
  "groups.error.slug": "Enter a slug",
  "groups.error.slugFormat":
    "Use 2-32 lowercase letters, digits and hyphens, starting and ending with a letter or digit",
  "groups.error.modelName": "Enter a model name",
  "groups.error.modelNameLength": "A model name can be at most {max} characters",
  "groups.error.modelNameWhitespace": "A model name can't contain spaces",
  "groups.error.modelNameDuplicate": "{name} is already a model of this group",
  "groups.error.modelsMax": "A group holds at most {max} models",
  "groups.error.modelNeedsTargets": "Every model needs at least one target",
  "groups.error.noActiveModel": "Add a model first — targets belong to a model",
  "groups.error.targetFormat": "Use a full model id in the form {example}",
  // A target may appear twice as long as the two entries use different
  // accounts, so the message says how to make the second one legal.
  "groups.error.targetDuplicate":
    "That model is already a target here on this account. Pick another account to add it again.",
  "groups.error.targetsMax": "A model holds at most {max} targets",
  "groups.error.save": "Couldn't save the group",
  "groups.error.delete": "Couldn't delete the group",

  // --- API keys -----------------------------------------------------------
  // --- CLI page (docs/cli.md, docs/admin-ui.md § CLI page) -----------------
  "cli.title": "CLI",
  "cli.devices.title": "CLI devices",
  "cli.devices.column.name": "Name",
  "cli.devices.column.lastSeen": "Last seen",
  "cli.devices.column.created": "Created",
  "cli.devices.revoked": "Revoked",
  "cli.devices.revoke": "Revoke",
  "cli.devices.revokeName": "Revoke {name}",
  "cli.devices.revokeConfirm":
    "Revoke \"{name}\"? It stops connecting within the hour and can only come back by signing in again.",
  "cli.providers.title": "CLI providers",
  "cli.providers.column.provider": "Provider",
  "cli.providers.column.state": "State",
  "cli.providers.column.models": "Models",
  "cli.providers.column.device": "Device",
  "cli.providers.connected": "Connected",
  "cli.providers.offline": "Offline",
  "cli.providers.model.count_one": "{count} model",
  "cli.providers.model.count_other": "{count} models",
  "cli.providers.noModels": "No models reported yet",
  "cli.providers.renameName": "Rename {name}",
  "cli.providers.removeName": "Remove {name}",
  "cli.providers.removeConfirm":
    "Remove \"{name}\"? Requests to {slug}/… stop resolving immediately.",
  "cli.providers.empty.title": "No CLI providers yet",
  "cli.providers.empty.body": "Register a local endpoint with kano-proxy add, then run kano-proxy start.",
  "cli.rename.title": "Rename provider",
  "cli.rename.label": "Display name",
  "cli.empty.title": "Bring your local models",
  "cli.empty.body":
    "Install the kano-proxy CLI and sign this machine in — anything OpenAI- or Anthropic-compatible running on it becomes a provider here, with no public address.",
  "cli.empty.install": "Install",
  "cli.empty.thenRun": "Then sign the machine in",
  "cli.empty.releases": "All platforms and checksums are on GitHub Releases",
  "cli.error.load": "Couldn't load your CLI devices",
  "cli.error.revoke": "Couldn't revoke the device",
  "cli.error.rename": "Couldn't rename the provider",
  "cli.error.remove": "Couldn't remove the provider",
  // --- CLI authorize view --------------------------------------------------
  "cli.authorize.title": "Authorize CLI device",
  "cli.authorize.question": "Sign in the device \"{device}\"?",
  "cli.authorize.hint": "Only approve if you just ran kano-proxy init on a machine you control.",
  "cli.authorize.approve": "Approve",
  "cli.authorize.deny": "Deny",
  "cli.authorize.codeTitle": "Enter this code in your terminal",
  "cli.authorize.codeHint": "The code works once and expires in 10 minutes.",
  "cli.authorize.alreadyApproved": "This request was already approved on another page.",
  "cli.authorize.denied": "Request denied. Nothing was signed in.",
  "cli.authorize.missing": "This sign-in request is unknown or has expired. Run kano-proxy init again to get a fresh link.",
  "cli.authorize.goToApp": "Go to Providers",
  "cli.authorize.error.approve": "Couldn't approve the request",
  "cli.authorize.error.deny": "Couldn't deny the request",

  "keys.title": "API keys",
  "keys.tab.keys": "Keys",
  "keys.tab.connect": "Connect",
  "keys.create": "Create key",
  "keys.namePlaceholder": "Key name",
  "keys.nameLabel": "Name",
  "keys.column.name": "Name",
  "keys.column.key": "Key",
  "keys.column.limit": "Spend",
  "keys.column.lastUsed": "Last used",
  "keys.editKey": "Edit {name}",
  "keys.dialog.editTitle": "Edit key",
  "keys.dialog.save": "Save changes",
  "keys.dialog.saveFailed": "Couldn't save the key",
  "keys.dialog.limitLabel": "Spend limit",
  "keys.dialog.limitOptional": "Optional",
  "keys.dialog.limitPlaceholder": "No limit",
  "keys.dialog.limitHint":
    "Requests stop once this key's estimated spend reaches the limit. Leave blank for no limit.",
  "keys.dialog.limitInvalid": "Enter an amount above zero, or leave it blank",
  "keys.dialog.intervalLabel": "Resets",
  "keys.dialog.interval.daily": "Daily",
  "keys.dialog.interval.weekly": "Weekly",
  "keys.dialog.interval.monthly": "Monthly",
  "keys.dialog.interval.total": "Never (all-time)",
  "keys.dialog.includeOauth": "Count subscription usage",
  "keys.dialog.includeOauthHint":
    "Include traffic through your connected subscriptions at their list prices. Custom endpoints always count.",
  "keys.dialog.currentSpend": "This key has spent {amount} in the current window.",
  "keys.revoke": "Revoke",
  "keys.revokeConfirm": "Revoke this key? Anything using it stops working immediately.",
  "keys.empty.title": "No keys yet",
  "keys.empty.body": "Create one to start calling your models.",
  "keys.created.title": "Key created",
  "keys.created.body": "Copy it now — it won't be shown again.",
  "keys.connect.title": "Connect a client",
  "keys.connect.openai": "OpenAI-compatible",
  "keys.connect.anthropic": "Anthropic-compatible",
  "keys.connect.auth": "Authorization",
  "keys.connect.authValue": "Send your key as a bearer token or as {header}",
  "keys.connect.modelHint": "Use model ids in the form {example}",
  "keys.error.load": "Couldn't load your keys",
  "keys.error.create": "Couldn't create the key",
  "keys.error.revoke": "Couldn't revoke the key",

  // --- Changelog ----------------------------------------------------------
  "changelog.title": "What's new",
  "changelog.currentShort": "Current",
  /* Long form of the badge above, rendered sr-only: a screen reader listing
     tags out of context gets "Current" with nothing to be current *of*. */
  "changelog.current": "You're on this version",
  "changelog.updateAvailable": "{version} is available",
  "changelog.noNotes": "No notes for this release.",
  "changelog.empty.title": "No releases yet",
  "changelog.empty.body": "Release notes show up here once the first one is published.",
  "changelog.unavailable.title": "Release notes unavailable",
  "changelog.unavailable.body": "This deployment isn't set up to show release notes.",
  "changelog.error.load": "Couldn't load release notes",

  // --- Login --------------------------------------------------------------
  "login.signIn": "Sign in",
  "login.lede": "Continue to your account.",
  "login.google": "Continue with Google",
  "login.pitch.title": "One endpoint for every coding agent.",
  "login.pitch.body":
    "Connect the AI subscriptions you already pay for, then point any OpenAI- or Anthropic-compatible client at a single URL.",
  "login.copyright": "© {year} {name}",
  "login.error": "Couldn't sign you in",
} as const
