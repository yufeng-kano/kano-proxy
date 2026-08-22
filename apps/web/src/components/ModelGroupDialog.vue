<script setup lang="ts">
/**
 * Create / edit a model group — since v4 a virtual endpoint: a `/g/<slug>/…`
 * base URL plus the models callable on it, each with its own ordered target
 * list (docs/admin-ui.md § Groups page).
 *
 * Three columns, one surface: ① identity (name, slug + live endpoint URL
 * preview, strategy), ② the picker, ③ the group's models. Column ③ is a stack
 * of model sections; exactly one is **active**, and the picker feeds it —
 * clicking a catalog model adds it as a target of the active model, pinned to
 * the selected account. The picker itself is unchanged from v3: an inverted L
 * of provider tabs, account rail, and the model list between them.
 *
 * **Every target this dialog creates pins an account** — there is no "Any
 * account" entry in the rail (docs, 2026-08-13). The wire still accepts
 * unpinned targets, and a group created before this design still renders its
 * unpinned targets with the "Any account" tag; the picker just cannot make
 * new ones. Duplicate identity is model + account within one group model,
 * exactly as the server checks it.
 *
 * Delete lives in the footer, same reasoning as key revoke: rare and
 * irreversible, so it costs opening the group first and confirming.
 */
import { computed, onMounted, ref, watch } from "vue"
import { useAccounts } from "@/composables/useAccounts"
import { useAuth } from "@/composables/useAuth"
import { useCustomProviders } from "@/composables/useCustomProviders"
import { useI18n } from "@/i18n"
import type { MessageKey } from "@/i18n"
import {
  ApiError,
  createModelGroup,
  deleteModelGroup,
  groupBaseUrls,
  updateModelGroup,
} from "@/services/api"
import {
  DEFAULT_ROUTING_STRATEGY,
  MODEL_GROUP_MODEL_NAME_MAX,
  MODEL_GROUP_MODELS_MAX,
  MODEL_GROUP_NAME_MAX,
  MODEL_GROUP_SLUG_RE,
  MODEL_GROUP_TARGETS_MAX,
  PROVIDERS,
  PROVIDER_IDS,
  ROUTING_STRATEGIES,
  type CatalogModel,
  type ModelGroup,
  type ModelGroupTarget,
  type ProviderAccount,
  type ProviderId,
  type RoutingStrategy,
} from "@/types"
import ActionIcon from "./ui/ActionIcon.vue"
import AppButton from "./ui/AppButton.vue"
import Badge from "./ui/Badge.vue"
import Banner from "./ui/Banner.vue"
import EmptyState from "./ui/EmptyState.vue"
import FormField from "./ui/FormField.vue"
import Modal from "./ui/Modal.vue"
import SectionNav from "./ui/SectionNav.vue"
import type { SectionItem } from "./ui/SectionNav.vue"
import TextInput from "./ui/TextInput.vue"

const props = defineProps<{
  /** null/omitted = create mode. A group = edit mode, prefilled from it. */
  group?: ModelGroup | null
  /** The catalog the page already holds — the picker's source, filtered client-side. */
  catalog: CatalogModel[]
}>()

const emit = defineEmits<{ close: []; saved: [] }>()

const { t } = useI18n()
const { user } = useAuth()
const accounts = useAccounts()
const customProviders = useCustomProviders()

/**
 * Provider display copy lives in the catalog and `PROVIDERS` carries only wire
 * ids, so an explicit map — a template literal widens to `string` and would not
 * typecheck against `MessageKey`, which is what makes a renamed key fail the
 * build here rather than render blank (same call as the Models page).
 */
const NAME_KEY: Record<ProviderId, MessageKey> = {
  "claude-code": "provider.claude-code.name",
  codex: "provider.codex.name",
  grok: "provider.grok.name",
  antigravity: "provider.antigravity.name",
}

/**
 * Each strategy's name and the line saying what it does to this group's
 * targets. Explicit maps for the same typing reason as above, and the seam a
 * second strategy lands in: an entry in each, an entry in `ROUTING_STRATEGIES`.
 */
const STRATEGY_KEY: Record<RoutingStrategy, MessageKey> = {
  ordered: "strategy.ordered",
}

const STRATEGY_HINT_KEY: Record<RoutingStrategy, MessageKey> = {
  ordered: "strategy.group.ordered",
}

/**
 * How many models the list draws at once. It scrolls, so this is only a ceiling
 * on what a large provider costs to render — the search is how anything past it
 * is reached.
 */
const MAX_MODELS = 50

const isEdit = computed(() => !!props.group)

/**
 * Rows carry a `uid` the group itself never has: sections and target rows are
 * keyed by it, so renames and re-pins edit the row in place instead of
 * replacing it — and take the control the user is holding down with them.
 */
type TargetRow = ModelGroupTarget & { uid: number }
type ModelRow = { uid: number; name: string; targets: TargetRow[] }

let nextUid = 0

function toTargetRow(target: ModelGroupTarget): TargetRow {
  return { ...target, uid: nextUid++ }
}

function toModelRow(model: { name: string; targets: ModelGroupTarget[] }): ModelRow {
  return { uid: nextUid++, name: model.name, targets: model.targets.map(toTargetRow) }
}

/** The display label — free text, and never part of the URL. */
const name = ref(props.group?.name ?? "")
/** The endpoint's URL id. Working copies: everything is only sent on Save. */
const slug = ref(props.group?.slug ?? "")
/** The group's models, each with its ordered targets. */
const models = ref<ModelRow[]>((props.group?.models ?? []).map(toModelRow))
/** The model the picker feeds. Defaults to the first, so edit mode starts live. */
const activeModelUid = ref<number | null>(models.value[0]?.uid ?? null)
const modelDraft = ref("")
/**
 * How the group orders its targets. A group saved before the field existed
 * (or read from a cache entry that predates it) means the server's default.
 */
const strategy = ref<RoutingStrategy>(props.group?.strategy ?? DEFAULT_ROUTING_STRATEGY)

/** Picker state: which provider tab, which account in its rail, what is typed. */
const selectedTab = ref<string>(
  prefixOf(props.group?.models[0]?.targets[0]?.model ?? "") || PROVIDERS[0]!.id,
)
const selectedAccountId = ref<string | null>(null)
const query = ref("")

const saving = ref(false)
const deleting = ref(false)
const nameError = ref<string | null>(null)
const slugError = ref<string | null>(null)
const modelsError = ref<string | null>(null)
const error = ref<string | null>(null)
/** The move buttons' only feedback for a screen-reader user. */
const announcement = ref("")

/**
 * Wire form, not copy: the shape of a target id is protocol (docs/api.md), so
 * it reads the same in every locale — same call as the `slug/*` preview in the
 * custom endpoint dialog.
 */
const MODEL_ID_FORM = "provider/model"

/**
 * A target's shape, mirroring the server's `splitModelId`: a prefix and an
 * upstream id either side of the first "/", both non-empty. The upstream half
 * may itself contain further slashes.
 */
function isTargetId(value: string): boolean {
  const slash = value.indexOf("/")
  return slash > 0 && slash < value.length - 1
}

/** Text before the first "/" — a builtin provider id, or a custom slug. */
function prefixOf(model: string): string {
  const slash = model.indexOf("/")
  return slash === -1 ? "" : model.slice(0, slash)
}

/** The builtin behind a tab key, or null when it is a custom endpoint's slug. */
function asBuiltin(key: string): ProviderId | null {
  return PROVIDER_IDS.includes(key as ProviderId) ? (key as ProviderId) : null
}

/* --- Endpoint preview ----------------------------------------------------- */

/**
 * The two live base URLs the typed slug produces — updated as the user types,
 * so the field answers "where will clients point" before Save. A blank slug
 * previews with a placeholder token rather than a broken URL.
 */
const endpointPreview = computed(() => {
  const value = slug.value.trim() || "<slug>"
  const urls = groupBaseUrls(value)
  return [
    { label: "OpenAI", url: urls.openai },
    { label: "Anthropic", url: urls.anthropic },
  ]
})

/* --- Providers, accounts, models (picker) --------------------------------- */

/** The tab strip: the builtins in their declared order, then each custom endpoint. */
const tabs = computed<SectionItem[]>(() => [
  ...PROVIDERS.map((p) => ({ id: p.id, label: t(NAME_KEY[p.id]) })),
  ...(customProviders.state.data ?? []).map((cp) => ({ id: cp.slug, label: cp.name })),
])

/**
 * A stored tab that no longer exists resolves to the first one rather than an
 * empty picker — the endpoint whose slug prefixed an existing target may have
 * been deleted since.
 */
const activeTab = computed(() =>
  tabs.value.some((tab) => tab.id === selectedTab.value) ? selectedTab.value : tabs.value[0]!.id,
)

const activeTabLabel = computed(
  () => tabs.value.find((tab) => tab.id === activeTab.value)?.label ?? "",
)

/** One rail entry: an account a target can pin, named the way the user named it. */
type RailAccount = { id: string; label: string; hint: string | null }

/** The user's own name wins, then the upstream identity, then a short id. */
function accountName(account: ProviderAccount): string {
  return account.custom_label || account.label || account.id.slice(0, 8)
}

/**
 * The rail for a provider key. A builtin's is its bound accounts in pool order;
 * a custom endpoint's is the single `upstream_accounts` row holding its API key
 * (`account_id` on the list response), named after the endpoint and hinted with
 * its key mask. Uniform either way, which is what lets a target pin one of
 * either without the rest of this dialog caring which it got.
 */
function railFor(providerKey: string): RailAccount[] {
  const builtin = asBuiltin(providerKey)
  if (builtin) {
    return (accounts.byProvider[builtin].data?.accounts ?? []).map((a) => ({
      id: a.id,
      label: accountName(a),
      hint: null,
    }))
  }
  const custom = (customProviders.state.data ?? []).find((cp) => cp.slug === providerKey)
  if (!custom?.account_id) return []
  return [{ id: custom.account_id, label: custom.name, hint: custom.key_mask }]
}

const rail = computed(() => railFor(activeTab.value))

/**
 * Whether the rail is still on its way. A *failed* load is not pending — the
 * empty state then says what it can rather than spinning forever.
 */
const railPending = computed(() => {
  const builtin = asBuiltin(activeTab.value)
  const state = builtin ? accounts.byProvider[builtin] : customProviders.state
  return !state.data && !state.error
})

const selectedAccount = computed(
  () => rail.value.find((a) => a.id === selectedAccountId.value) ?? null,
)

const trimmedQuery = computed(() => query.value.trim())

/**
 * The active tab's models. Keyed on each row's own `provider`, which is the
 * catalog's section.
 *
 * `display_name` is a **search key only** — the list renders the id alone
 * (docs/admin-ui.md § Groups page), but "Opus 4.8" still has to find
 * `claude-opus-4-8`, so the friendly name stays in the match.
 */
const tabModels = computed<CatalogModel[]>(() => {
  const q = trimmedQuery.value.toLowerCase()
  const out: CatalogModel[] = []
  for (const model of props.catalog) {
    if (model.provider !== activeTab.value || !isTargetId(model.id)) continue
    if (q && !model.id.toLowerCase().includes(q) && !model.display_name.toLowerCase().includes(q)) {
      continue
    }
    out.push(model)
    if (out.length === MAX_MODELS) break
  }
  return out
})

/* --- Loading ------------------------------------------------------------- */

const requestedProviders = new Set<ProviderId>()
let requestedCustom = false

/**
 * Accounts are fetched per provider, once, and only for a tab the user actually
 * opens — the composables are cache-first, so a recent visit to Providers costs
 * nothing here.
 */
function ensureRail(providerKey: string) {
  const builtin = asBuiltin(providerKey)
  if (!builtin) {
    if (requestedCustom) return
    requestedCustom = true
    void customProviders.load()
    return
  }
  if (requestedProviders.has(builtin)) return
  requestedProviders.add(builtin)
  void accounts.loadProvider(builtin)
}

onMounted(() => {
  const uid = user.value?.id ?? null
  accounts.setUserId(uid)
  customProviders.setUserId(uid)
  // The tab strip needs the endpoint list before it can offer their tabs.
  requestedCustom = true
  void customProviders.load()
  ensureRail(activeTab.value)
  // An existing target whose pin no longer resolves is re-picked in place,
  // which needs that provider's accounts even if its tab is never opened.
  for (const model of models.value) {
    for (const target of model.targets) {
      if (target.account_id) ensureRail(prefixOf(target.model))
    }
  }
})

watch(activeTab, (key) => {
  selectedAccountId.value = null
  query.value = ""
  ensureRail(key)
})

/**
 * One account is not a choice. Selecting it saves a click that could only ever
 * have one outcome — a custom endpoint always has exactly one key — and the
 * rail still shows which account is selected, so nothing is decided silently.
 */
watch(
  rail,
  (list) => {
    if (!selectedAccountId.value && list.length === 1) selectedAccountId.value = list[0]!.id
  },
  { immediate: true },
)

/* --- Group models --------------------------------------------------------- */

const activeModel = computed<ModelRow | null>(
  () => models.value.find((m) => m.uid === activeModelUid.value) ?? null,
)

/**
 * One callable model name, by the server's rule: 1-128 chars, no whitespace —
 * `/` is fine, a group endpoint has no provider/model resolution to collide
 * with (docs/providers.md § Model groups).
 */
function modelNameError(value: string): string | null {
  if (!value) return t("groups.error.modelName")
  if (value.length > MODEL_GROUP_MODEL_NAME_MAX) {
    return t("groups.error.modelNameLength", { max: MODEL_GROUP_MODEL_NAME_MAX })
  }
  if (/\s/.test(value)) return t("groups.error.modelNameWhitespace")
  return null
}

/** Adds one model section and makes it active, so the next pick lands in it. */
function commitModelDraft() {
  const value = modelDraft.value.trim()
  modelsError.value = null
  if (!value) return
  const invalid = modelNameError(value)
  if (invalid) {
    modelsError.value = invalid
    return
  }
  if (models.value.some((m) => m.name.trim() === value)) {
    modelsError.value = t("groups.error.modelNameDuplicate", { name: value })
    return
  }
  if (models.value.length >= MODEL_GROUP_MODELS_MAX) {
    modelsError.value = t("groups.error.modelsMax", { max: MODEL_GROUP_MODELS_MAX })
    return
  }
  const row: ModelRow = { uid: nextUid++, name: value, targets: [] }
  models.value.push(row)
  activeModelUid.value = row.uid
  modelDraft.value = ""
}

function removeModel(uid: number) {
  const idx = models.value.findIndex((m) => m.uid === uid)
  if (idx === -1) return
  models.value.splice(idx, 1)
  modelsError.value = null
  if (activeModelUid.value === uid) activeModelUid.value = models.value[0]?.uid ?? null
}

/* --- Targets (of the active model) ---------------------------------------- */

/**
 * Identity the server dedupes on: model *and* account together, within one
 * group model. The separator is a NUL — never legal inside a model id or an
 * account id — written as an escape so this file stays plain text, exactly as
 * the server writes it.
 */
function identityOf(target: { model: string; account_id: string | null }): string {
  return `${target.model}\u0000${target.account_id ?? ""}`
}

/**
 * What a pinned target's account is called right now. The loaded rail wins over
 * `account_label`: the label is what the *last read* resolved, and the rail is
 * live — it also names an account the server had no label for at all, which
 * would otherwise read as removed. `null` only when nothing knows it any more.
 */
function pinnedLabel(target: ModelGroupTarget): string | null {
  if (!target.account_id) return null
  const known = railFor(prefixOf(target.model)).find((a) => a.id === target.account_id)
  return known ? known.label : target.account_label
}

/**
 * A pin whose account is gone: neither the provider's rail nor the server's
 * read-time label knows it any more (docs/auth.md § Model groups). The target
 * is skipped at request time, so the row warns and offers a re-pick — dropping
 * the pin silently would change what the group routes to without saying so.
 */
function isMissingAccount(target: ModelGroupTarget): boolean {
  return !!target.account_id && !pinnedLabel(target)
}

/** Already in the active model's list, on the account the rail has selected. */
function isAdded(modelId: string): boolean {
  const active = activeModel.value
  if (!active) return false
  const identity = identityOf({ model: modelId, account_id: selectedAccountId.value })
  return active.targets.some((t) => identityOf(t) === identity)
}

/** Adds the model pinned to the selected account, into the active group model. */
function addTarget(modelId: string) {
  const value = modelId.trim()
  const account = selectedAccount.value
  modelsError.value = null
  if (!account) return
  const active = activeModel.value
  if (!active) {
    // The picker has nowhere to put the pick — say so where the models live.
    modelsError.value = t("groups.error.noActiveModel")
    return
  }
  if (!isTargetId(value)) {
    modelsError.value = t("groups.error.targetFormat", { example: MODEL_ID_FORM })
    return
  }
  const target: ModelGroupTarget = {
    model: value,
    account_id: account.id,
    account_label: account.label,
  }
  if (active.targets.some((t) => identityOf(t) === identityOf(target))) {
    // Rejected here rather than at save: the list is what the user is reading,
    // so the moment to say "already in this model" is the moment they add it.
    modelsError.value = t("groups.error.targetDuplicate")
    return
  }
  if (active.targets.length >= MODEL_GROUP_TARGETS_MAX) {
    modelsError.value = t("groups.error.targetsMax", { max: MODEL_GROUP_TARGETS_MAX })
    return
  }
  active.targets.push(toTargetRow(target))
}

/**
 * Re-pinning a broken target in place, rather than remove-and-re-add: the pin
 * is the only part that went bad, and the target's position in the order is
 * worth keeping. A change that would collide with another row is refused and
 * the row keeps what it had — the same rule the server applies on Save, applied
 * where the user can see it. Returns whether the change was taken.
 */
function setAccount(model: ModelRow, index: number, accountId: string): boolean {
  const target = model.targets[index]
  if (!target) return false
  const next = accountId || null
  if (next === target.account_id) return true

  const candidate = { ...target, account_id: next }
  if (model.targets.some((t, i) => i !== index && identityOf(t) === identityOf(candidate))) {
    modelsError.value = t("groups.error.targetDuplicate")
    return false
  }

  modelsError.value = null
  target.account_id = next
  // The label is display data the server resolves; keep it in step locally so
  // the row stops reading as a removed account the moment it is re-pinned.
  const picked = railFor(prefixOf(target.model)).find((a) => a.id === next)
  target.account_label = picked ? picked.label : null
  return true
}

/**
 * A refused pick must not stay on screen. The native select has already moved
 * to the new option, and Vue patches nothing back because the bound value never
 * changed — so the element is put back by hand.
 */
function onAccountChange(model: ModelRow, index: number, event: Event) {
  const el = event.target as HTMLSelectElement
  if (!setAccount(model, index, el.value)) el.value = model.targets[index]?.account_id ?? ""
}

/**
 * The re-pick options for a broken row: the provider's accounts, plus the
 * unresolvable pin itself so the select never silently drops it. That entry is
 * disabled — the only way out is forward.
 */
function repickOptions(target: ModelGroupTarget) {
  const options = railFor(prefixOf(target.model)).map((a) => ({
    value: a.id,
    label: a.label,
    disabled: false,
  }))
  if (target.account_id && !options.some((o) => o.value === target.account_id)) {
    options.unshift({
      value: target.account_id,
      label: t("groups.account.missing"),
      disabled: true,
    })
  }
  return options
}

function removeTarget(model: ModelRow, index: number) {
  model.targets.splice(index, 1)
  modelsError.value = null
  announcement.value = ""
}

function move(model: ModelRow, from: number, to: number) {
  if (to < 0 || to >= model.targets.length) return
  const [moved] = model.targets.splice(from, 1)
  if (!moved) return
  model.targets.splice(to, 0, moved)
  announcement.value = t("groups.dialog.moved", {
    target: moved.model,
    position: to + 1,
    total: model.targets.length,
  })
}

const canSave = computed(
  () => models.value.length > 0 && models.value.every((m) => m.targets.length > 0),
)

/* --- Save / delete -------------------------------------------------------- */

/** Mirrors the server rule so a violation never costs a round trip. */
function validateName(): string | null {
  const value = name.value.trim()
  if (!value) return t("groups.error.name")
  if (value.length > MODEL_GROUP_NAME_MAX) {
    return t("groups.error.nameLength", { max: MODEL_GROUP_NAME_MAX })
  }
  return null
}

function validateSlug(): string | null {
  const value = slug.value.trim()
  if (!value) return t("groups.error.slug")
  if (!MODEL_GROUP_SLUG_RE.test(value) || value.length < 2 || value.length > 32) {
    return t("groups.error.slugFormat")
  }
  return null
}

/** The model set, by the server's rules — names valid and unique in the group. */
function validateModels(): string | null {
  const seen = new Set<string>()
  for (const model of models.value) {
    const value = model.name.trim()
    const invalid = modelNameError(value)
    if (invalid) return invalid
    if (seen.has(value)) return t("groups.error.modelNameDuplicate", { name: value })
    seen.add(value)
  }
  return null
}

/**
 * A rejected write answers with a single `error` string rather than a field map
 * (docs/auth.md § Model groups), so the message is placed by what it is about:
 * anything naming the slug lands on the slug field, anything naming the group's
 * name lands on the name field, anything naming a model or target lands under
 * the model list, and everything else is a banner.
 */
function applyServerError(e: unknown, fallback: string) {
  const message = e instanceof ApiError && e.status === 400 ? e.message : null
  if (!message) {
    error.value = fallback
    return
  }
  if (/^slug\b/i.test(message)) slugError.value = message
  else if (/^name\b|^a model group named/i.test(message)) nameError.value = message
  else if (/^models?\b|^duplicate model|^targets?\b|^duplicate target/i.test(message)) {
    modelsError.value = message
  } else error.value = message
}

async function submit() {
  if (saving.value || !canSave.value) return
  nameError.value = null
  slugError.value = null
  modelsError.value = null
  error.value = null

  // A model name typed but not yet committed is one the user means to save;
  // taking it here beats silently dropping it because they reached for Save
  // instead of Enter. A rejected one stops the save with its own message —
  // and a fresh model has no targets yet, which the canSave gate below the
  // commit would block, so only commit when the draft is non-empty.
  if (modelDraft.value.trim()) {
    commitModelDraft()
    if (modelsError.value) return
  }

  const invalidName = validateName()
  if (invalidName) {
    nameError.value = invalidName
    return
  }
  const invalidSlug = validateSlug()
  if (invalidSlug) {
    slugError.value = invalidSlug
    return
  }
  const invalidModels = validateModels()
  if (invalidModels) {
    modelsError.value = invalidModels
    return
  }
  if (!canSave.value) {
    modelsError.value = t("groups.error.modelNeedsTargets")
    return
  }

  saving.value = true
  try {
    const body = {
      name: name.value.trim(),
      slug: slug.value.trim(),
      // `account_label` is read-only display data — it goes no further than
      // this dialog.
      models: models.value.map((m) => ({
        name: m.name.trim(),
        targets: m.targets.map((t) => ({ model: t.model, account_id: t.account_id })),
      })),
      strategy: strategy.value,
    }
    if (props.group) await updateModelGroup(props.group.id, body)
    else await createModelGroup(body)
    emit("saved")
    emit("close")
  } catch (e) {
    applyServerError(e, t("groups.error.save"))
  } finally {
    saving.value = false
  }
}

async function remove() {
  if (!props.group || deleting.value) return
  if (!confirm(t("groups.deleteConfirm"))) return
  error.value = null
  deleting.value = true
  try {
    await deleteModelGroup(props.group.id)
    emit("saved")
    emit("close")
  } catch {
    error.value = t("groups.error.delete")
  } finally {
    deleting.value = false
  }
}
</script>

<template>
  <Modal
    size="wide"
    :title="isEdit ? t('groups.dialog.editTitle') : t('groups.create')"
    @close="emit('close')"
  >
    <!-- Three columns, one surface: identity, picker, models — a hairline
         between each adjacent pair, all three heads on the same row, the same
         padding in every column. Boxing them separately read as unrelated
         cards and was rejected; so did stacking identity above the picker. -->
    <div class="board">
      <!-- ① What the group is called, where clients point (the slug and the
           URLs it produces), and how it picks among each model's targets. -->
      <section class="col" aria-labelledby="group-col-identity">
        <h3 id="group-col-identity" class="col-head">
          {{ t("groups.dialog.identityLabel") }}
        </h3>

        <FormField
          v-slot="field"
          hint-above
          :label="t('groups.dialog.nameLabel')"
          :hint="t('groups.dialog.nameHint')"
          :error="nameError ?? undefined"
        >
          <TextInput
            :id="field.id"
            v-model="name"
            :placeholder="t('groups.dialog.namePlaceholder')"
            :described-by="field.describedBy"
            :invalid="field.invalid"
            :disabled="saving || deleting"
            @enter="submit"
          />
        </FormField>

        <FormField
          v-slot="field"
          hint-above
          :label="t('groups.dialog.slugLabel')"
          :hint="t('groups.dialog.slugHint')"
          :error="slugError ?? undefined"
        >
          <TextInput
            :id="field.id"
            v-model="slug"
            mono
            :placeholder="t('groups.dialog.slugPlaceholder')"
            :described-by="field.describedBy"
            :invalid="field.invalid"
            :disabled="saving || deleting"
            @enter="submit"
          />
        </FormField>

        <!-- The URLs the slug produces, live as the user types — wire values,
             not copy, so they render the same in every locale. -->
        <div class="endpoint-preview">
          <p v-for="entry in endpointPreview" :key="entry.label" class="endpoint-line">
            <span class="endpoint-kind">{{ entry.label }}</span>
            <code class="mono endpoint-url" :title="entry.url">{{ entry.url }}</code>
          </p>
          <p class="field-hint">{{ t("groups.dialog.slugMoves") }}</p>
        </div>

        <!-- How the group picks among its targets. One option today, and the
             select still renders: it is the seam future strategies appear in,
             and it says the group has a routing policy at all. -->
        <FormField
          v-slot="field"
          hint-above
          :label="t('strategy.label')"
          :hint="t(STRATEGY_HINT_KEY[strategy])"
        >
          <select
            :id="field.id"
            v-model="strategy"
            class="select strategy"
            :aria-describedby="field.describedBy"
            :disabled="saving || deleting"
          >
            <option v-for="option in ROUTING_STRATEGIES" :key="option" :value="option">
              {{ t(STRATEGY_KEY[option]) }}
            </option>
          </select>
        </FormField>
      </section>

      <!-- ② Where targets come from: provider, then account, then model. -->
      <section class="col" aria-labelledby="group-col-picker">
        <h3 id="group-col-picker" class="col-head">{{ t("groups.dialog.pickerLabel") }}</h3>

        <div class="picker">
          <!-- Scrolls sideways rather than wrapping, so the region below never
               moves as the endpoint list grows. -->
          <SectionNav
            class="picker-tabs"
            :items="tabs"
            :active="activeTab"
            :label="t('groups.dialog.providersLabel')"
            @select="selectedTab = $event"
          />

          <!-- The panel the tabs point at: SectionNav's `aria-controls` is
               `panel-<id>`, and only the selected one is ever in the DOM. -->
          <div
            :id="`panel-${activeTab}`"
            class="picker-body"
            role="tabpanel"
            :aria-label="activeTabLabel"
          >
            <!-- The rail. Buttons, not a listbox: each one is a filter the
                 model list answers, and its pressed state is the selection. -->
            <div class="rail" role="group" :aria-label="t('groups.dialog.accountsLabel')">
              <p v-if="railPending" class="rail-note" role="status">{{ t("app.loading") }}</p>
              <p v-else-if="!rail.length" class="rail-note">
                {{ t("groups.dialog.accountsEmpty") }}
              </p>
              <button
                v-for="account in rail"
                :key="account.id"
                type="button"
                class="rail-item"
                :class="{ active: account.id === selectedAccountId }"
                :aria-pressed="account.id === selectedAccountId"
                :disabled="saving || deleting"
                @click="selectedAccountId = account.id"
              >
                <span class="rail-name">{{ account.label }}</span>
                <span v-if="account.hint" class="rail-hint mono">{{ account.hint }}</span>
              </button>
            </div>

            <!-- The models an account can run. -->
            <div class="models">
              <template v-if="selectedAccount">
                <label class="models-search">
                  <span class="sr-only">{{ t("action.search") }}</span>
                  <TextInput
                    v-model="query"
                    type="search"
                    :placeholder="t('groups.dialog.searchPlaceholder')"
                    :disabled="saving || deleting"
                  />
                </label>

                <ul v-if="tabModels.length" class="models-list">
                  <li v-for="model in tabModels" :key="model.id">
                    <!-- The accessible name carries both halves of what the
                         click means: this model, on this account. -->
                    <AppButton
                      size="sm"
                      variant="ghost"
                      class="model"
                      :label="
                        t('groups.dialog.addModelOn', {
                          model: model.id,
                          account: selectedAccount.label,
                        })
                      "
                      :disabled="saving || deleting"
                      @click="addTarget(model.id)"
                    >
                      <!-- The id and nothing else. What the column cannot fit
                           truncates and stays readable on the `title`; the
                           button's own name carries it in full for a screen
                           reader. -->
                      <code class="mono model-id" :title="model.id">{{ model.id }}</code>
                      <Badge v-if="isAdded(model.id)" tone="ok">
                        {{ t("groups.dialog.added") }}
                      </Badge>
                    </AppButton>
                  </li>
                </ul>
                <p v-else-if="trimmedQuery" class="note">
                  {{ t("groups.dialog.noMatches", { query: trimmedQuery }) }}
                </p>
                <p v-else class="note">{{ t("groups.dialog.modelsEmpty") }}</p>
              </template>

              <EmptyState
                v-else-if="!railPending && !rail.length"
                compact
                :title="t('groups.dialog.railEmpty.title')"
                :body="t('groups.dialog.railEmpty.body', { page: t('providers.title') })"
              />

              <p v-else-if="!railPending" class="note pick-note">
                {{ t("groups.dialog.pickAccount") }}
              </p>
            </div>
          </div>
        </div>
      </section>

      <!-- ③ The group's models, each with its ordered targets. One is active
           — the one the picker feeds. -->
      <section class="col" aria-labelledby="group-col-models">
        <h3 id="group-col-models" class="col-head">{{ t("groups.dialog.modelsLabel") }}</h3>

        <!-- The way in: name a model clients will send, then feed it targets. -->
        <div class="add-model">
          <label class="add-model-field">
            <span class="sr-only">{{ t("groups.dialog.modelField") }}</span>
            <TextInput
              v-model="modelDraft"
              mono
              :placeholder="t('groups.dialog.modelPlaceholder')"
              :invalid="!!modelsError"
              :disabled="saving || deleting"
              @enter="commitModelDraft"
            />
          </label>
          <AppButton
            size="sm"
            :label="
              modelDraft.trim()
                ? t('groups.dialog.addModelNamed', { name: modelDraft.trim() })
                : undefined
            "
            :disabled="!modelDraft.trim() || saving || deleting"
            @click="commitModelDraft"
          >
            {{ t("groups.dialog.addModel") }}
          </AppButton>
        </div>

        <p v-if="modelsError" class="field-error" role="alert">{{ modelsError }}</p>

        <p v-if="!models.length" class="field-hint">{{ t("groups.dialog.modelsHint") }}</p>

        <!-- The model sections. Exactly one is active (radio semantics — the
             picker feeds one model at a time); the active one is marked by a
             fill plus a weight step, never color alone. -->
        <div v-if="models.length" class="model-sections" role="radiogroup" :aria-label="t('groups.dialog.modelsLabel')">
          <section
            v-for="model in models"
            :key="model.uid"
            class="model-section"
            :class="{ active: model.uid === activeModelUid }"
            @click="activeModelUid = model.uid"
          >
            <div class="model-head">
              <input
                :id="`model-active-${model.uid}`"
                class="model-radio"
                type="radio"
                name="active-model"
                :checked="model.uid === activeModelUid"
                :disabled="saving || deleting"
                @change="activeModelUid = model.uid"
              />
              <label class="sr-only" :for="`model-active-${model.uid}`">
                {{ t("groups.dialog.selectModel", { name: model.name }) }}
              </label>
              <label class="sr-only" :for="`model-name-${model.uid}`">
                {{ t("groups.dialog.modelNameLabel", { name: model.name }) }}
              </label>
              <TextInput
                :id="`model-name-${model.uid}`"
                v-model="model.name"
                mono
                class="model-name"
                :disabled="saving || deleting"
                @focus="activeModelUid = model.uid"
              />
              <AppButton
                size="sm"
                variant="ghost"
                :label="t('groups.dialog.removeModel', { name: model.name })"
                :disabled="saving || deleting"
                @click.stop="removeModel(model.uid)"
              >
                {{ t("action.remove") }}
              </AppButton>
            </div>

            <!-- The position is the routing rule, so it is real text in the
                 row rather than a list marker: `list-style: none` drops list
                 semantics in Safari, and the number must survive. -->
            <ol v-if="model.targets.length" class="targets">
              <li v-for="(target, index) in model.targets" :key="target.uid" class="target">
                <span class="pos tabular">{{ index + 1 }}</span>
                <code class="mono target-id" :title="target.model">{{ target.model }}</code>

                <div class="target-actions">
                  <AppButton
                    icon-only
                    size="sm"
                    variant="ghost"
                    :label="t('groups.dialog.moveUp', { target: target.model })"
                    :disabled="index === 0 || saving || deleting"
                    @click.stop="move(model, index, index - 1)"
                  >
                    <template #icon><ActionIcon name="arrow-up" /></template>
                  </AppButton>
                  <AppButton
                    icon-only
                    size="sm"
                    variant="ghost"
                    :label="t('groups.dialog.moveDown', { target: target.model })"
                    :disabled="index === model.targets.length - 1 || saving || deleting"
                    @click.stop="move(model, index, index + 1)"
                  >
                    <template #icon><ActionIcon name="arrow-down" /></template>
                  </AppButton>
                  <!-- Labelled, not a glyph: the row's subject goes in the
                       accessible name because "Remove" repeats down the list. -->
                  <AppButton
                    size="sm"
                    variant="ghost"
                    :label="t('groups.dialog.removeTarget', { target: target.model })"
                    :disabled="saving || deleting"
                    @click.stop="removeTarget(model, index)"
                  >
                    {{ t("action.remove") }}
                  </AppButton>
                </div>

                <!-- Per-target facts sit in their own line under the id: the
                     account today, weight and live usage when balancing lands. -->
                <div class="target-facts">
                  <template v-if="isMissingAccount(target)">
                    <Badge tone="warn">{{ t("groups.account.missing") }}</Badge>
                    <label class="sr-only" :for="`target-account-${target.uid}`">
                      {{ t("groups.dialog.accountLabel", { target: target.model }) }}
                    </label>
                    <select
                      :id="`target-account-${target.uid}`"
                      class="select repick"
                      :value="target.account_id ?? ''"
                      :disabled="saving || deleting"
                      @change="onAccountChange(model, index, $event)"
                    >
                      <option
                        v-for="option in repickOptions(target)"
                        :key="option.value"
                        :value="option.value"
                        :disabled="option.disabled"
                      >
                        {{ option.label }}
                      </option>
                    </select>
                    <span class="target-warning">{{ t("groups.account.skipped") }}</span>
                  </template>
                  <Badge v-else-if="target.account_id" tone="neutral">
                    {{ pinnedLabel(target) }}
                  </Badge>
                  <!-- Made before this design, or by the API directly: the
                       whole pool, still valid, still shown for what it is. -->
                  <Badge v-else tone="neutral">{{ t("groups.account.any") }}</Badge>
                </div>
              </li>
            </ol>
            <p v-else class="field-hint section-hint">{{ t("groups.dialog.targetsEmpty") }}</p>
          </section>
        </div>

        <Banner v-if="error" tone="error">{{ error }}</Banner>

        <!-- Outside the list above so it survives its rerender. -->
        <span class="sr-only" role="status" aria-live="polite">{{ announcement }}</span>
      </section>
    </div>

    <template #footer>
      <!-- Edit only, and pushed to the far end of the footer: a destructive
           action must not sit against Save where a mis-aimed click lands. -->
      <AppButton
        v-if="isEdit"
        class="delete"
        variant="danger"
        :loading="deleting"
        :disabled="saving"
        @click="remove"
      >
        {{ t("groups.delete") }}
      </AppButton>
      <AppButton variant="ghost" :disabled="saving || deleting" @click="emit('close')">
        {{ t("action.cancel") }}
      </AppButton>
      <AppButton
        variant="primary"
        :loading="saving"
        :disabled="!canSave || deleting"
        @click="submit"
      >
        {{ isEdit ? t("groups.dialog.save") : t("groups.create") }}
      </AppButton>
    </template>
  </Modal>
</template>

<style scoped>
/*
 * Three columns that read as one surface.
 *
 * The dialog's own background runs behind all three, a hairline separates each
 * adjacent pair, and nothing inside any column is boxed — structure is drawn
 * with hairlines only. The board bleeds through Modal's body padding (which it
 * then restates per column, so the spacing is unchanged) because those dividers
 * have to reach the panel's edges.
 *
 * The columns stretch to a common height — the default, deliberately not
 * `align-items: start` — so each divider runs the full height of the surface
 * rather than stopping wherever the shortest column ends.
 *
 * Tracks: identity is a form and needs the least; the picker holds a rail *and*
 * a list; the models column now holds names + targets + controls, so it takes
 * the widest track (the v4 reweighting behind the wider `wide` panel).
 */
.board {
  display: grid;
  grid-template-columns: minmax(0, 0.75fr) minmax(0, 1.15fr) minmax(0, 1.3fr);
  margin: calc(var(--space-5) * -1);
  /* Shared height of the two scrolling regions (picker body, model sections).
     Viewport-driven, not fixed: the wide panel grows with the screen, and a
     fixed height left half the dialog empty on a tall display. The 380px
     budget is everything that is not this region: overlay padding, panel
     header/footer, column padding, the column head, and the search row. */
  --pane-h: clamp(400px, calc(100dvh - 380px), 760px);
}

.col {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  min-width: 0;
  padding: var(--space-5);
}

/* Models is last: drop its bottom pad so a full list meets the footer the
   picker meets (bottom-bleed). Heads stay on one row — top pad unchanged. */
.col:last-child {
  padding-bottom: 0;
}

/* The dividers — one per adjacent pair, and nothing else draws a line between
   columns. */
.col + .col {
  border-left: 1px solid var(--border);
}

/* Each head is its column's first child under identical padding, so all three
   sit on one row without any of them declaring a height. */
.col-head {
  margin: 0;
  color: var(--text);
  font-size: var(--text-sm);
  font-weight: var(--weight-semibold);
  letter-spacing: var(--tracking-tight);
}

/* --- Endpoint preview ----------------------------------------------------- */

.endpoint-preview {
  display: grid;
  gap: var(--space-1);
}

.endpoint-line {
  display: flex;
  align-items: baseline;
  gap: var(--space-2);
  margin: 0;
  min-width: 0;
}

.endpoint-kind {
  flex-shrink: 0;
  color: var(--text-secondary);
  font-size: var(--text-2xs);
}

.endpoint-url {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text);
  font-size: var(--text-xs);
}

/* --- Picker -------------------------------------------------------------- */

/* Unframed: the tab strip's underline and the rail's edge are the only lines,
   which is what makes the inverted L read as structure on the surface rather
   than as a widget dropped onto it.

   Full bleed: the picker cancels its column's horizontal *and* bottom padding
   so the tabs, rail, and model list run divider to divider and the rail meets
   the panel footer's top rule. No `gap`: a gutter between the tab strip and
   the body is a break in the L. */
.picker {
  display: flex;
  flex-direction: column;
  flex: 1;
  margin: 0 calc(var(--space-5) * -1) calc(var(--space-5) * -1);
  min-width: 0;
  min-height: 0;
}

.picker-tabs {
  flex-shrink: 0;
  border-bottom: 1px solid var(--border);
}

/* The inverted L: rail down the left, models filling the rest. A declared
   height, not a content-driven one — the two regions scroll inside it, so the
   dialog's own height never depends on how many models a provider lists. */
.picker-body {
  display: grid;
  grid-template-columns: clamp(96px, 28%, 128px) minmax(0, 1fr);
  flex: 1 1 auto;
  height: var(--pane-h);
  min-height: var(--pane-h);
}

.rail {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: var(--space-1) 0;
  border-right: 1px solid var(--border);
  overflow-y: auto;
  overscroll-behavior: contain;
}

/* A full-bleed list row, not a card: the selection fill runs the rail's whole
   width, edge to edge. */
.rail-item {
  display: flex;
  flex-direction: column;
  gap: 1px;
  flex-shrink: 0;
  width: 100%;
  padding: var(--space-2) var(--space-3);
  border: none;
  border-radius: 0;
  background: transparent;
  color: var(--text-secondary);
  text-align: left;
  cursor: pointer;
  transition:
    background var(--duration-fast) var(--ease),
    color var(--duration-fast) var(--ease);
}

.rail-item:hover {
  background: var(--hover);
  color: var(--text);
}

/* Selection is the full-width fill *and* a weight step — the fill alone is a
   ~2% luminance delta and reads as noise (docs/admin-ui.md § Scales). */
.rail-item.active {
  background: var(--hover);
  color: var(--text);
  font-weight: var(--weight-medium);
}

.rail-item:disabled {
  cursor: not-allowed;
  opacity: 0.6;
}

.rail-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: var(--text-xs);
}

.rail-hint {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--faint);
  font-size: var(--text-2xs);
}

.rail-note {
  margin: 0;
  padding: var(--space-2);
  color: var(--muted);
  font-size: var(--text-2xs);
  line-height: 1.5;
}

.models {
  display: flex;
  flex-direction: column;
  min-width: 0;
  min-height: 0;
}

/* Flush like everything else in this column: a full-bleed row with a hairline
   under it, not a rounded field inset in a flush region. */
.models-search {
  display: flex;
  flex-shrink: 0;
  padding: 0 var(--space-1);
  border-bottom: 1px solid var(--border);
}

/* The field gives up its border, radius and fill; without them it is the row,
   and the row is what the picker already is. */
.models-search :deep(.control) {
  padding: 0 var(--space-2);
  border: none;
  border-radius: 0;
  background: transparent;
}

/* Focus moves to the row, since the field no longer has a box to ring. */
.models-search :deep(.control:focus) {
  box-shadow: none;
}

.models-search:focus-within {
  box-shadow: inset 0 0 0 2px var(--ring-border);
}

/* The one region that grows with its data, so it is the one that scrolls —
   vertically, and only vertically. See the v3 notes: the 0-min declared track
   plus `min-width: 0` on items is what stops the sideways scroll. */
.models-list {
  display: grid;
  grid-template-columns: minmax(0, 1fr);
  align-content: start;
  gap: 2px;
  flex: 1;
  margin: 0;
  padding: var(--space-1);
  min-height: 0;
  overflow-y: auto;
  overscroll-behavior: contain;
  list-style: none;
}

.models-list > li {
  min-width: 0;
}

/* A full-width row rather than a pill: the id is what the user reads down the
   list, so the button is shaped to it. */
.models-list :deep(.model) {
  width: 100%;
  justify-content: flex-start;
}

.models-list :deep(.model .btn-label) {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: var(--space-3);
  width: 100%;
  min-width: 0;
  font-weight: var(--weight-normal);
}

.model-id {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.note {
  margin: 0;
  padding: var(--space-2);
  color: var(--muted);
  font-size: var(--text-2xs);
  line-height: 1.5;
  overflow-wrap: anywhere;
}

/* Nothing picked yet: the region says what to do instead of standing empty. */
.pick-note {
  margin: auto;
  text-align: center;
}

/* --- Group models (column ③) ---------------------------------------------- */

.add-model {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  width: 100%;
}

.add-model-field {
  display: block;
  flex: 1;
  min-width: 0;
}

.field-hint,
.field-error {
  margin: 0;
  font-size: var(--text-2xs);
  line-height: 1.5;
  overflow-wrap: anywhere;
}

.field-hint {
  color: var(--muted);
}

.field-error {
  color: var(--danger);
}

/* The stack of model sections scrolls as one region, flush to the column
   divider and the panel edge — the same cancel the picker uses — so section
   hairlines run edge to edge. Content stays inset per row. */
.model-sections {
  display: flex;
  flex-direction: column;
  margin: 0 calc(var(--space-5) * -1);
  max-height: var(--pane-h);
  overflow-y: auto;
  overscroll-behavior: contain;
}

.model-section {
  padding: var(--space-2) 0 var(--space-1);
}

.model-section + .model-section {
  border-top: 1px solid var(--border);
}

/* The active section — the one the picker feeds — carries the same fill +
   weight convention as the rail's selection: never color alone (the radio
   beside the name states it for assistive tech). */
.model-section.active {
  background: var(--hover);
}

.model-head {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  padding: 0 var(--space-5);
}

.model-radio {
  flex-shrink: 0;
  margin: 0;
  accent-color: var(--accent, currentColor);
}

.model-head :deep(.model-name) {
  flex: 1;
  min-width: 0;
}

.model-section.active .model-head :deep(input) {
  font-weight: var(--weight-medium);
}

.section-hint {
  padding: var(--space-1) var(--space-5) var(--space-2);
}

/* --- Target rows ----------------------------------------------------------- */

.targets {
  display: grid;
  align-content: start;
  margin: var(--space-1) 0 0;
  padding: 0;
  list-style: none;
}

/* Three tracks — position, subject, controls — and a second line under the
   subject for the row's facts. Rows are separated by a hairline rather than
   each being boxed. */
.target {
  display: grid;
  grid-template-columns: 16px minmax(0, 1fr) auto;
  align-items: center;
  gap: var(--space-1) var(--space-2);
  /* Horizontal inset restates the column gutter the sections cancelled, so
     numbers / ids / buttons stay aligned with the column head while the
     hairline runs edge to edge. */
  padding: var(--space-2) var(--space-5);
}

.target + .target {
  border-top: 1px solid var(--border);
}

/* Sized to two digits — the list stops at 20 — so the ids line up in a column
   rather than shifting by one character at position 10. */
.pos {
  grid-column: 1;
  text-align: right;
  color: var(--faint);
  font-size: var(--text-2xs);
}

.target-id {
  grid-column: 2;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text);
}

.target-actions {
  display: flex;
  align-items: center;
  gap: var(--space-1);
  grid-column: 3;
  grid-row: 1;
}

.target-facts {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  flex-wrap: wrap;
  grid-column: 2 / -1;
  min-width: 0;
}

/* TextInput's control spec at the small size, so the re-pick select sits level
   with the ghost buttons sharing its row. */
.select {
  max-width: 100%;
  min-width: 0;
  height: 28px;
  padding: 0 var(--space-2);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-sm);
  background: var(--surface);
  color: var(--text);
  font-size: var(--text-xs);
}

/* The re-pick borrows the warn tone from the badge beside it: the row is
   telling the user something is wrong, and the control is the fix. */
.select.repick {
  border-color: var(--warn-border);
}

/* In the identity column it is a form field like the name input above it, so
   it takes TextInput's full-size spec instead of the row-sized one. */
.select.strategy {
  width: 100%;
  height: 34px;
  padding: 0 var(--space-3);
  font-size: var(--text-sm);
}

.select:focus {
  border-color: var(--ring-border);
  box-shadow: var(--ring);
  outline: none;
}

.select:disabled {
  background: var(--surface-2);
  color: var(--muted);
  cursor: not-allowed;
}

.target-warning {
  color: var(--warn);
  font-size: var(--text-2xs);
  overflow-wrap: anywhere;
}

/* The footer packs its buttons to the right; the auto margin is what separates
   Delete from Cancel/Save rather than a gap nobody else in the app has. */
.delete {
  margin-right: auto;
}

/* Below the table breakpoint three columns no longer fit, so they stack in
   reading order — identity, picker, models — and the dividers turn with them,
   staying one hairline between each pair. */
@media (max-width: 768px) {
  .board {
    grid-template-columns: minmax(0, 1fr);
  }

  .col + .col {
    border-left: none;
    border-top: 1px solid var(--border);
  }

  .picker-body {
    flex: none;
    height: 300px;
    min-height: 0;
  }

  .model-sections {
    max-height: 338px;
  }
}

/* Below the sheet breakpoint the inverted L flattens too: the rail becomes a
   chip strip under the tabs, because even 96px of side rail is width a phone
   does not have to give. */
@media (max-width: 640px) {
  .picker-body {
    grid-template-columns: minmax(0, 1fr);
    grid-template-rows: auto minmax(0, 1fr);
    flex: none;
    height: 300px;
    min-height: 0;
  }

  .rail {
    flex-direction: row;
    align-items: center;
    gap: var(--space-1);
    padding: var(--space-2);
    border-right: none;
    border-bottom: 1px solid var(--border);
    overflow-x: auto;
    overflow-y: hidden;
    scrollbar-width: none;
  }

  .rail::-webkit-scrollbar {
    display: none;
  }

  /* Chips again here: a horizontal strip needs each item's own outline back,
     since there is no rail edge for a full-bleed fill to run to. */
  .rail-item {
    width: auto;
    min-height: 34px;
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    white-space: nowrap;
  }

  .rail-name,
  .rail-hint {
    overflow: visible;
  }
}

/* Touch targets: the select matches what a small button gets here, and 16px
   type so iOS Safari does not zoom the sheet when it takes focus. */
@media (pointer: coarse) {
  .select {
    min-height: 34px;
    font-size: var(--text-md);
  }

  .select.strategy {
    min-height: 40px;
    font-size: var(--text-md);
  }
}
</style>
