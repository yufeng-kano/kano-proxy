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
  // Visible text for the row actions. Their accessible names name the row's
  // subject too (`providers.account.*`, `custom.*Endpoint`) — the same three
  // words repeat down a list, so the label alone does not say which account.
  // Those names must contain these words verbatim (WCAG 2.5.3).
  "action.rename": "Rename",
  "action.remove": "Remove",
  "action.edit": "Edit",
  "action.test": "Test",
  "action.search": "Search",
  "action.done": "Done",

  // --- Shared states ------------------------------------------------------
  "state.copyFailed": "Couldn't copy to the clipboard",
  "state.never": "Never",

  // --- Status -------------------------------------------------------------
  "status.active": "Active",
  "status.standby": "Standby",
  "status.benched": "Paused",
  "status.unusable": "Needs attention",

  // --- Overview (dashboard) ----------------------------------------------
  "overview.title": "Overview",
  "overview.subtitle": "Your traffic across every connected provider",
  "overview.range.24h": "24 hours",
  "overview.range.7d": "7 days",
  "overview.range.30d": "30 days",
  "overview.range.label": "Time range",
  "overview.range.short.24h": "24h",
  "overview.range.short.7d": "7d",
  "overview.range.short.30d": "30d",
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

  // --- Providers ----------------------------------------------------------
  "providers.title": "Providers",
  "providers.subtitle": "Connect the accounts your requests route through",
  "providers.all": "All",
  "providers.group.custom": "Custom",
  "providers.addAccount": "Add account",
  // One gate per section, so its name is the section's — not a row's.
  "providers.section.edit": "Edit {section}",
  "providers.section.doneEditing": "Done editing {section}",
  // Accessible names; the buttons' visible text is `providers.account.primary`
  // and `action.rename` / `action.remove`, which these repeat verbatim.
  "providers.account.promote": "Make {name} primary",
  "providers.account.primary": "Primary",
  "providers.account.rename": "Rename {name}",
  "providers.account.remove": "Remove {name}",
  "providers.account.removeConfirm":
    "Remove this account? Requests will stop routing through it.",
  "providers.account.noUsage": "Usage will appear once this account is used",
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
  "providers.error.promote": "Couldn't change the primary account",
  "providers.error.remove": "Couldn't remove the account",
  "providers.error.rename": "Couldn't rename the account",

  // --- Provider descriptions (product copy, not mechanism) ----------------
  "provider.claude-code.name": "Claude Code",
  "provider.claude-code.blurb": "Your Anthropic subscription",
  "provider.codex.name": "Codex",
  "provider.codex.blurb": "Your ChatGPT subscription",
  "provider.grok.name": "Grok",
  "provider.grok.blurb": "Your xAI subscription",
  "provider.custom.name": "Custom endpoints",
  "provider.custom.blurb": "Any OpenAI- or Anthropic-compatible API",

  // --- Add account dialog -------------------------------------------------
  "addAccount.title": "Add {provider} account",
  "addAccount.intro": "Sign in with your {provider} account to add it to the pool.",
  "addAccount.start": "Continue",
  "addAccount.manual": "Enter tokens manually",
  "addAccount.openAuth": "Open sign-in page",
  "addAccount.claude.step1": "Approve access on the page that opened.",
  "addAccount.claude.step2": "Copy the code you're given and paste it below.",
  "addAccount.claude.label": "Authorization code",
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
  "custom.error.apiKey": "Enter an API key",
  "custom.error.save": "Couldn't save the endpoint",
  "custom.error.remove": "Couldn't remove the endpoint",
  "custom.error.load": "Couldn't load your endpoints",

  // --- Models -------------------------------------------------------------
  "models.title": "Models",
  "models.subtitle": "Everything you can call right now",
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
  "models.section.groups": "Groups",
  "models.error.load": "Couldn't load models",

  // --- Groups -------------------------------------------------------------
  "groups.title": "Groups",
  "groups.subtitle": "Your own model names, pointed at real models",
  "groups.create": "Create group",
  "groups.column.name": "Name",
  "groups.column.targets": "Targets",
  "groups.column.updated": "Updated",
  // Accessible names for the row controls: several rows offer the same two
  // words, so the group's name goes in the name. Each contains the visible
  // label verbatim (WCAG 2.5.3) — the name itself, and "Edit".
  "groups.copyName": "Copy {name}",
  "groups.editGroup": "Edit {name}",
  "groups.empty.title": "No groups yet",
  "groups.empty.body":
    "A group gives a model your own name. Point a client's fixed model id at whatever you actually run, or gather one model from several accounts and endpoints behind a single name.",
  "groups.delete": "Delete group",
  "groups.deleteConfirm": "Delete this group? Requests using its name stop working immediately.",
  "groups.dialog.editTitle": "Edit group",
  "groups.dialog.save": "Save changes",
  "groups.dialog.nameLabel": "Name",
  "groups.dialog.namePlaceholder": "my-model",
  "groups.dialog.nameHint": "Clients send this as the model id, on both base URLs.",
  "groups.dialog.targetsLabel": "Targets",
  "groups.dialog.targetsHint":
    "Requests try these in order and use the first one that can take them. A target pinned to an account uses only that account; the rest use the whole provider.",
  "groups.dialog.targetsEmpty": "Pick an account, then choose the models it should serve.",
  // The account dimension. "Any account" is the unpinned state the picker no
  // longer creates but older groups still hold — the provider's own pool and
  // priority — and it reads the same on the list page and here.
  "groups.account.any": "Any account",
  "groups.account.missing": "Account removed",
  "groups.account.skipped": "Skipped until you pick another account",
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
  // The way in for ids no catalog lists — the whole of codex, and anything an
  // upstream added since the catalog was cached.
  "groups.dialog.manualLabel": "Model id",
  "groups.dialog.manualPlaceholder": "model id",
  "groups.dialog.manualHint": "Not listed? Type the id the upstream uses.",
  "groups.dialog.manualPreview": "Adds {id}",
  "groups.dialog.add": "Add",
  "groups.dialog.addTarget": "Add {target}",
  "groups.dialog.noMatches": "No models match \"{query}\"",
  "groups.dialog.moveUp": "Move {target} up",
  "groups.dialog.moveDown": "Move {target} down",
  "groups.dialog.moved": "{target} is now {position} of {total}",
  "groups.dialog.removeTarget": "Remove {target}",
  "groups.error.load": "Couldn't load your groups",
  "groups.error.name": "Enter a name",
  "groups.error.nameLength": "Use {max} characters or fewer",
  "groups.error.nameWhitespace": "A name can't contain spaces",
  "groups.error.nameSlash": "A name can't contain \"/\"",
  "groups.error.targetFormat": "Use a full model id in the form {example}",
  // A model may appear twice as long as the two entries use different
  // accounts, so the message says how to make the second one legal.
  "groups.error.targetDuplicate":
    "That model is already in this group on this account. Pick another account to add it again.",
  "groups.error.targetsMax": "A group holds at most {max} models",
  "groups.error.save": "Couldn't save the group",
  "groups.error.delete": "Couldn't delete the group",

  // --- API keys -----------------------------------------------------------
  "keys.title": "API keys",
  "keys.subtitle": "Keys your clients use to reach your models",
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
  "keys.connect.body": "Point any OpenAI- or Anthropic-compatible client at these URLs.",
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
  "changelog.subtitle": "Every release, newest first",
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
