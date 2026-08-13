<script setup lang="ts">
/**
 * Create / edit a model group (docs/admin-ui.md § Groups page).
 *
 * Two panes: **pick on the left, order on the right.** Building a group is
 * browsing — which provider, whose account, which model — so the left pane is a
 * picker and the right pane is the ordered list the picker feeds. The order is
 * the routing priority, which is why the right pane is built with move controls
 * rather than checkboxes, and why saving always sends the whole list.
 *
 * The picker is an inverted L: a horizontally scrollable provider tab strip
 * across the top (the Models page pattern, reused as-is), an account rail down
 * the left edge, and the model list filling the rest. Tab → accounts (lazily
 * loaded, cache-first, the same data the Providers page reads); account →
 * models, filtered client-side over the catalog the page already holds so a
 * keystroke never hits the network, plus a free-text row for ids the catalog
 * does not list (codex lists none, so that row is the only way in for it).
 *
 * **Every target this dialog creates pins an account** — there is no "Any
 * account" entry in the rail (docs, 2026-08-13). The wire still accepts
 * unpinned targets, and a group created before this design still renders its
 * unpinned targets on the right with the "Any account" tag; the picker just
 * cannot make new ones. Duplicate identity is therefore model + account,
 * exactly as the server checks it, and "same model, different account" is two
 * clicks: pick account A, click the model, pick account B, click it again.
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
  updateModelGroup,
} from "@/services/api"
import {
  MODEL_GROUP_ALIASES_MAX,
  MODEL_GROUP_ALIAS_MAX,
  MODEL_GROUP_NAME_MAX,
  MODEL_GROUP_TARGETS_MAX,
  PROVIDERS,
  PROVIDER_IDS,
  type CatalogModel,
  type ModelGroup,
  type ModelGroupTarget,
  type ProviderAccount,
  type ProviderId,
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
}

/**
 * How many models the list draws at once. It scrolls, so this is only a ceiling
 * on what a large provider costs to render — the search is how anything past it
 * is reached.
 */
const MAX_MODELS = 50

const isEdit = computed(() => !!props.group)

/**
 * A row carries a `uid` the group itself never has: the right pane is keyed by
 * it, so re-pinning a broken target edits that row instead of replacing it —
 * and takes the control the user is holding down with it.
 */
type TargetRow = ModelGroupTarget & { uid: number }

let nextUid = 0

function toRow(target: ModelGroupTarget): TargetRow {
  return { ...target, uid: nextUid++ }
}

/** The display label — free text, and never what a client sends. */
const name = ref(props.group?.name ?? "")
/** The callable ids. Working copies: both lists are only sent on Save. */
const aliases = ref<string[]>([...(props.group?.aliases ?? [])])
const aliasDraft = ref("")
const targets = ref<TargetRow[]>((props.group?.targets ?? []).map(toRow))

/** Picker state: which provider tab, which account in its rail, what is typed. */
const selectedTab = ref<string>(prefixOf(props.group?.targets[0]?.model ?? "") || PROVIDERS[0]!.id)
const selectedAccountId = ref<string | null>(null)
const query = ref("")
const manual = ref("")

const saving = ref(false)
const deleting = ref(false)
const nameError = ref<string | null>(null)
const aliasError = ref<string | null>(null)
const targetError = ref<string | null>(null)
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

/* --- Providers, accounts, models ----------------------------------------- */

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
 * catalog's section — so the fixed `group` section never appears under a
 * provider tab, and a group can never target another group.
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

/**
 * What the free-text row would add. Typing the upstream id is enough — the tab
 * already says which provider it belongs to — and a value that already carries
 * the prefix is left alone rather than prefixed twice.
 */
const manualId = computed(() => {
  const raw = manual.value.trim()
  if (!raw) return ""
  return raw.startsWith(`${activeTab.value}/`) ? raw : `${activeTab.value}/${raw}`
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
  // An existing target whose pin no longer resolves is re-picked on the right
  // pane, which needs that provider's accounts even if its tab is never opened.
  for (const target of targets.value) {
    if (target.account_id) ensureRail(prefixOf(target.model))
  }
})

watch(activeTab, (key) => {
  selectedAccountId.value = null
  query.value = ""
  manual.value = ""
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

/* --- Targets ------------------------------------------------------------- */

/**
 * Identity the server dedupes on: model *and* account together. The separator
 * is a NUL — never legal inside a model id or an account id — written as an
 * escape so this file stays plain text, exactly as the server writes it.
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

/** Already in the list, on the account the rail currently has selected. */
function isAdded(modelId: string): boolean {
  const identity = identityOf({ model: modelId, account_id: selectedAccountId.value })
  return targets.value.some((t) => identityOf(t) === identity)
}

/** Adds the model pinned to the selected account — the only kind this dialog makes. */
function addTarget(modelId: string) {
  const value = modelId.trim()
  const account = selectedAccount.value
  targetError.value = null
  if (!account) return
  if (!isTargetId(value)) {
    targetError.value = t("groups.error.targetFormat", { example: MODEL_ID_FORM })
    return
  }
  const target: ModelGroupTarget = {
    model: value,
    account_id: account.id,
    account_label: account.label,
  }
  if (targets.value.some((t) => identityOf(t) === identityOf(target))) {
    // Rejected here rather than at save: the list is what the user is reading,
    // so the moment to say "already in this group" is the moment they add it.
    targetError.value = t("groups.error.targetDuplicate")
    return
  }
  if (targets.value.length >= MODEL_GROUP_TARGETS_MAX) {
    targetError.value = t("groups.error.targetsMax", { max: MODEL_GROUP_TARGETS_MAX })
    return
  }
  targets.value.push(toRow(target))
}

function addManual() {
  if (!manualId.value) return
  const before = targets.value.length
  addTarget(manualId.value)
  if (targets.value.length > before) manual.value = ""
}

/**
 * Re-pinning a broken target in place, rather than remove-and-re-add: the pin
 * is the only part that went bad, and the target's position in the order is
 * worth keeping. A change that would collide with another row is refused and
 * the row keeps what it had — the same rule the server applies on Save, applied
 * where the user can see it. Returns whether the change was taken.
 */
function setAccount(index: number, accountId: string): boolean {
  const target = targets.value[index]
  if (!target) return false
  const next = accountId || null
  if (next === target.account_id) return true

  const candidate = { ...target, account_id: next }
  if (targets.value.some((t, i) => i !== index && identityOf(t) === identityOf(candidate))) {
    targetError.value = t("groups.error.targetDuplicate")
    return false
  }

  targetError.value = null
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
function onAccountChange(index: number, event: Event) {
  const el = event.target as HTMLSelectElement
  if (!setAccount(index, el.value)) el.value = targets.value[index]?.account_id ?? ""
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

function removeTarget(index: number) {
  targets.value.splice(index, 1)
  targetError.value = null
  announcement.value = ""
}

function move(from: number, to: number) {
  if (to < 0 || to >= targets.value.length) return
  const [moved] = targets.value.splice(from, 1)
  if (!moved) return
  targets.value.splice(to, 0, moved)
  announcement.value = t("groups.dialog.moved", {
    target: moved.model,
    position: to + 1,
    total: targets.value.length,
  })
}

const canSave = computed(() => targets.value.length > 0)

/* --- Aliases ------------------------------------------------------------- */

/**
 * One alias, by the server's rule: 1-128 chars, no whitespace, no "/" — the
 * missing slash is what keeps a bare id from ever colliding with a
 * `provider/model` one.
 */
function aliasRuleError(alias: string): string | null {
  if (alias.length > MODEL_GROUP_ALIAS_MAX) {
    return t("groups.error.aliasLength", { max: MODEL_GROUP_ALIAS_MAX })
  }
  if (/\s/.test(alias)) return t("groups.error.aliasWhitespace")
  if (alias.includes("/")) return t("groups.error.aliasSlash")
  return null
}

/** Takes one alias into the chip list. Returns whether it was taken. */
function addAlias(raw: string): boolean {
  const value = raw.trim()
  aliasError.value = null
  if (!value) return false
  const invalid = aliasRuleError(value)
  if (invalid) {
    aliasError.value = invalid
    return false
  }
  if (aliases.value.includes(value)) {
    aliasError.value = t("groups.error.aliasDuplicate", { alias: value })
    return false
  }
  if (aliases.value.length >= MODEL_GROUP_ALIASES_MAX) {
    aliasError.value = t("groups.error.aliasesMax", { max: MODEL_GROUP_ALIASES_MAX })
    return false
  }
  aliases.value.push(value)
  return true
}

function commitAliasDraft() {
  if (addAlias(aliasDraft.value)) aliasDraft.value = ""
}

/**
 * A comma commits too, so a pasted `a,b,c` becomes three chips. Everything up
 * to the last comma is committed; whatever follows stays in the field. A part
 * the rules reject stops the run and stays in the field with the rest of it, so
 * nothing is dropped on the way to an error message.
 */
watch(aliasDraft, (value) => {
  if (!value.includes(",")) return
  const parts = value.split(",")
  let i = 0
  for (; i < parts.length - 1; i++) {
    const part = parts[i]!.trim()
    if (!part) continue
    if (!addAlias(part)) break
  }
  aliasDraft.value = parts.slice(i).join(",")
})

function removeAlias(alias: string) {
  aliases.value = aliases.value.filter((a) => a !== alias)
  aliasError.value = null
}

/* --- Save / delete ------------------------------------------------------- */

/** Mirrors the server rule so a violation never costs a round trip. */
function validateName(): string | null {
  const value = name.value.trim()
  if (!value) return t("groups.error.name")
  if (value.length > MODEL_GROUP_NAME_MAX) {
    return t("groups.error.nameLength", { max: MODEL_GROUP_NAME_MAX })
  }
  return null
}

/**
 * A rejected write answers with a single `error` string rather than a field map
 * (docs/auth.md § Model groups), so the message is placed by what it is about:
 * anything naming an alias lands under the chips — including the cross-group
 * conflict, whose text names the alias it clashed with — anything naming the
 * group's name lands on the name field, anything naming a target lands under
 * the target list, and everything else is a banner.
 */
function applyServerError(e: unknown, fallback: string) {
  const message = e instanceof ApiError && e.status === 400 ? e.message : null
  if (!message) {
    error.value = fallback
    return
  }
  if (/^alias(es)?\b|^duplicate alias/i.test(message)) aliasError.value = message
  else if (/^name\b|^a model group named/i.test(message)) nameError.value = message
  else if (/^targets?\b|^duplicate target/i.test(message)) targetError.value = message
  else error.value = message
}

async function submit() {
  if (saving.value || !canSave.value) return
  nameError.value = null
  aliasError.value = null
  targetError.value = null
  error.value = null

  // An alias typed but not yet committed is one the user means to save; taking
  // it here beats silently dropping it because they reached for Save instead of
  // Enter. A rejected one stops the save with its own message.
  if (aliasDraft.value.trim()) {
    commitAliasDraft()
    if (aliasError.value) return
  }

  const invalidName = validateName()
  if (invalidName) {
    nameError.value = invalidName
    return
  }
  if (!aliases.value.length) {
    aliasError.value = t("groups.error.aliasesEmpty")
    return
  }

  saving.value = true
  try {
    const body = {
      name: name.value.trim(),
      aliases: [...aliases.value],
      // `account_label` is read-only display data — it goes no further than
      // this dialog.
      targets: targets.value.map((t) => ({ model: t.model, account_id: t.account_id })),
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
    <!-- Three columns, one surface: identity, picker, order — a hairline
         between each adjacent pair, all three heads on the same row, the same
         padding in every column. Boxing them separately read as unrelated
         cards and was rejected; so did stacking identity above the picker. -->
    <div class="board">
      <!-- ① What the group is called, and what clients may call it. -->
      <section class="col" aria-labelledby="group-col-identity">
        <h3 id="group-col-identity" class="col-head">
          {{ t("groups.dialog.identityLabel") }}
        </h3>

        <FormField
          v-slot="field"
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

        <!-- The callable ids. A fieldset so the chips and the add field reach
             assistive tech as one named group. -->
        <fieldset class="fieldset">
          <legend class="field-label">{{ t("groups.dialog.aliasesLabel") }}</legend>
          <p class="field-hint">{{ t("groups.dialog.aliasesHint") }}</p>

          <ul v-if="aliases.length" class="chips">
            <li v-for="alias in aliases" :key="alias" class="chip">
              <code class="mono chip-id">{{ alias }}</code>
              <button
                type="button"
                class="chip-remove"
                :aria-label="t('groups.dialog.removeAlias', { alias })"
                :disabled="saving || deleting"
                @click="removeAlias(alias)"
              >
                <ActionIcon name="close" />
              </button>
            </li>
          </ul>

          <div class="alias-add">
            <label class="alias-field">
              <span class="sr-only">{{ t("groups.dialog.aliasField") }}</span>
              <TextInput
                v-model="aliasDraft"
                mono
                :placeholder="t('groups.dialog.aliasPlaceholder')"
                :invalid="!!aliasError"
                :disabled="saving || deleting"
                @enter="commitAliasDraft"
              />
            </label>
            <AppButton
              size="sm"
              :label="
                aliasDraft.trim()
                  ? t('groups.dialog.addAlias', { alias: aliasDraft.trim() })
                  : undefined
              "
              :disabled="!aliasDraft.trim() || saving || deleting"
              @click="commitAliasDraft"
            >
              {{ t("groups.dialog.add") }}
            </AppButton>
          </div>

          <p v-if="aliasError" class="field-error" role="alert">{{ aliasError }}</p>
        </fieldset>
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

            <!-- The models an account can run, plus the way in for the ids no
                 catalog lists. -->
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
                      <code class="mono model-id">{{ model.id }}</code>
                      <Badge v-if="isAdded(model.id)" tone="ok">
                        {{ t("groups.dialog.added") }}
                      </Badge>
                      <span v-else class="model-name">{{ model.display_name }}</span>
                    </AppButton>
                  </li>
                </ul>
                <p v-else-if="trimmedQuery" class="note">
                  {{ t("groups.dialog.noMatches", { query: trimmedQuery }) }}
                </p>
                <p v-else class="note">{{ t("groups.dialog.modelsEmpty") }}</p>

                <!-- Free text, always available: the server validates only the
                     prefix, so an id the catalog never listed is legitimate. -->
                <div class="manual">
                  <label class="manual-field">
                    <span class="sr-only">{{ t("groups.dialog.manualLabel") }}</span>
                    <TextInput
                      v-model="manual"
                      mono
                      :placeholder="t('groups.dialog.manualPlaceholder')"
                      :disabled="saving || deleting"
                      @enter="addManual"
                    />
                  </label>
                  <AppButton
                    size="sm"
                    :label="manualId ? t('groups.dialog.addTarget', { target: manualId }) : undefined"
                    :disabled="!manualId || saving || deleting"
                    @click="addManual"
                  >
                    {{ t("groups.dialog.add") }}
                  </AppButton>
                </div>
                <p class="note manual-note">
                  <template v-if="manualId">
                    {{ t("groups.dialog.manualPreview", { id: manualId }) }}
                  </template>
                  <template v-else>{{ t("groups.dialog.manualHint") }}</template>
                </p>
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

      <!-- ③ What the group actually routes to, in the order it is tried. -->
      <section class="col" aria-labelledby="group-col-targets">
        <h3 id="group-col-targets" class="col-head">{{ t("groups.dialog.targetsLabel") }}</h3>

        <!-- A real fieldset/legend so the list's name reaches assistive tech
             natively, matching the endpoint dialog's models block. -->
        <fieldset class="fieldset">
          <legend class="sr-only">{{ t("groups.dialog.targetsLabel") }}</legend>
          <p class="field-hint">{{ t("groups.dialog.targetsHint") }}</p>

          <!-- The position is the routing rule, so it is real text in the row
               rather than a list marker: `list-style: none` drops list semantics
               in Safari, and the number is the one thing that must survive. -->
          <ol v-if="targets.length" class="targets">
            <li v-for="(target, index) in targets" :key="target.uid" class="target">
              <span class="pos tabular">{{ index + 1 }}</span>
              <code class="mono target-id" :title="target.model">{{ target.model }}</code>

              <div class="target-actions">
                <AppButton
                  icon-only
                  size="sm"
                  variant="ghost"
                  :label="t('groups.dialog.moveUp', { target: target.model })"
                  :disabled="index === 0 || saving || deleting"
                  @click="move(index, index - 1)"
                >
                  <template #icon><ActionIcon name="arrow-up" /></template>
                </AppButton>
                <AppButton
                  icon-only
                  size="sm"
                  variant="ghost"
                  :label="t('groups.dialog.moveDown', { target: target.model })"
                  :disabled="index === targets.length - 1 || saving || deleting"
                  @click="move(index, index + 1)"
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
                  @click="removeTarget(index)"
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
                    class="select"
                    :value="target.account_id ?? ''"
                    :disabled="saving || deleting"
                    @change="onAccountChange(index, $event)"
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
                <!-- Made before this design, or by the API directly: the whole
                     pool, still valid, still shown for what it is. -->
                <Badge v-else tone="neutral">{{ t("groups.account.any") }}</Badge>
              </div>
            </li>
          </ol>
          <p v-else class="field-hint">{{ t("groups.dialog.targetsEmpty") }}</p>

          <p v-if="targetError" class="field-error" role="alert">{{ targetError }}</p>
        </fieldset>

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
 * have to reach the panel's edges: a rule floating inside a padded box is
 * exactly what made an earlier version read as cards parked next to each other.
 *
 * The columns stretch to a common height — the default, deliberately not
 * `align-items: start` — so each divider runs the full height of the surface
 * rather than stopping wherever the shortest column ends.
 *
 * Tracks: identity is a form and needs the least; the picker holds a rail *and*
 * a list and needs the most; the order column sits between them, wide enough
 * that a target's id and its three controls share one line.
 */
.board {
  display: grid;
  grid-template-columns: minmax(0, 0.8fr) minmax(0, 1.3fr) minmax(0, 1.05fr);
  margin: calc(var(--space-5) * -1);
}

.col {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  min-width: 0;
  padding: var(--space-5);
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

/* --- Picker -------------------------------------------------------------- */

/* Unframed: the tab strip's underline and the rail's edge are the only lines,
   which is what makes the inverted L read as structure on the surface rather
   than as a widget dropped onto it. It owns its column, so it takes the height
   that column has. */
.picker {
  display: flex;
  flex-direction: column;
  flex: 1;
  gap: var(--space-2);
  min-width: 0;
  min-height: 0;
}

.picker-tabs {
  flex-shrink: 0;
  border-bottom: 1px solid var(--border);
}

/* The inverted L: rail down the left, models filling the rest. A declared
   height, not a content-driven one — the two regions scroll inside it, so the
   dialog's own height never depends on how many models a provider lists. Its
   own column now, so it can be tall: 400px keeps the whole dialog inside a
   900px-tall viewport (Modal caps the panel at 760px, less its header, footer,
   and this column's padding).

   The rail is a proportion between two bounds rather than a fixed 128px: on a
   small desktop the panel is narrower than 3/4 of a wide one, and a fixed rail
   would take that difference out of the model list, which has less to give. */
.picker-body {
  display: grid;
  grid-template-columns: clamp(96px, 28%, 128px) minmax(0, 1fr);
  height: 400px;
  min-height: 0;
}

.rail {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: var(--space-2);
  border-right: 1px solid var(--border);
  overflow-y: auto;
  overscroll-behavior: contain;
}

.rail-item {
  display: flex;
  flex-direction: column;
  gap: 1px;
  flex-shrink: 0;
  padding: var(--space-2);
  border: 1px solid transparent;
  border-radius: var(--radius-sm);
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

/* Selection is a filled pill *and* a weight step — the fill alone is a ~2%
   luminance delta and reads as noise (docs/admin-ui.md § Scales). */
.rail-item.active {
  background: var(--hover);
  border-color: var(--border-strong);
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

.models-search {
  display: block;
  flex-shrink: 0;
  padding: var(--space-2);
  border-bottom: 1px solid var(--border);
}

/* The one region that grows with its data, so it is the one that scrolls. */
.models-list {
  display: grid;
  gap: 2px;
  flex: 1;
  margin: 0;
  padding: var(--space-1);
  min-height: 0;
  overflow-y: auto;
  overscroll-behavior: contain;
  list-style: none;
}

/* A full-width row rather than a pill: the id is what the user reads down the
   list, so the button is shaped to it. Selected through the list so these
   outrank AppButton's own single-class rules rather than depending on style
   order. */
.models-list :deep(.model) {
  width: 100%;
  justify-content: flex-start;
}

/* The default slot lands in one flex item, so the id/name split is set up
   inside it. */
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

.model-name {
  flex-shrink: 0;
  color: var(--muted);
  font-size: var(--text-2xs);
}

.manual {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  flex-shrink: 0;
  padding: var(--space-2);
  border-top: 1px solid var(--border);
}

.manual-field {
  display: block;
  flex: 1;
  min-width: 0;
}

.note {
  margin: 0;
  padding: var(--space-2);
  color: var(--muted);
  font-size: var(--text-2xs);
  line-height: 1.5;
  overflow-wrap: anywhere;
}

.manual-note {
  flex-shrink: 0;
  padding-top: 0;
}

/* Nothing picked yet: the region says what to do instead of standing empty. */
.pick-note {
  margin: auto;
  text-align: center;
}

/* --- Selected targets ---------------------------------------------------- */

/* A fieldset's default margin, padding, and border are browser chrome this
   design does not use — the legend alone carries the grouping. */
.fieldset {
  display: grid;
  gap: var(--space-2);
  margin: 0;
  padding: 0;
  border: none;
  min-width: 0;
}

.field-label {
  /* A legend carries its own inline padding in every engine. */
  padding: 0;
  color: var(--text-secondary);
  font-size: var(--text-xs);
  font-weight: var(--weight-medium);
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

.targets {
  display: grid;
  align-content: start;
  margin: 0;
  padding: 0;
  /* Matched to the picker beside it, so a full list and a full model list end
     at the same line instead of one column dragging the panel taller. */
  max-height: 424px;
  overflow-y: auto;
  overscroll-behavior: contain;
  list-style: none;
}

/* --- Aliases ------------------------------------------------------------- */

.chips {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-1);
  margin: 0;
  padding: 0;
  list-style: none;
}

/* The chip is the id plus the one thing that can be done to it, so the border
   belongs to the pair — this is a control, not a panel. */
.chip {
  display: inline-flex;
  align-items: center;
  gap: var(--space-1);
  max-width: 100%;
  padding: 1px 1px 1px var(--space-2);
  border: 1px solid var(--border);
  border-radius: var(--radius-full);
  background: var(--surface-2);
}

.chip-id {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text);
  font-size: var(--text-xs);
}

.chip-remove {
  display: grid;
  place-items: center;
  width: 20px;
  height: 20px;
  flex-shrink: 0;
  border: none;
  border-radius: var(--radius-full);
  background: transparent;
  color: var(--faint);
  cursor: pointer;
  transition:
    background var(--duration-fast) var(--ease),
    color var(--duration-fast) var(--ease);
}

.chip-remove:hover {
  background: var(--hover);
  color: var(--text);
}

.chip-remove:disabled {
  cursor: not-allowed;
  opacity: 0.6;
}

.chip-remove :deep(svg) {
  width: 12px;
  height: 12px;
}

.alias-add {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  width: 100%;
}

.alias-field {
  display: block;
  flex: 1;
  min-width: 0;
}

/* Three tracks — position, subject, controls — and a second line under the
   subject for the row's facts. The facts line is where per-target balancing
   lands later (weight, live usage) beside the account, so growing it costs a
   chip rather than a new row shape.

   Rows are separated by a hairline rather than each being boxed: twenty
   outlined rectangles inside a column is the same "card" noise the two-panel
   layout was rejected for. */
.target {
  display: grid;
  grid-template-columns: 16px minmax(0, 1fr) auto;
  align-items: center;
  gap: var(--space-1) var(--space-2);
  padding: var(--space-2) 0;
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
  border: 1px solid var(--warn-border);
  border-radius: var(--radius-sm);
  background: var(--surface);
  color: var(--text);
  font-size: var(--text-xs);
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
   reading order — identity, picker, order — and the dividers turn with them,
   staying one hairline between each pair. The picker gives back some height
   here: it is now one region in a scrolling column rather than one of three
   side by side. */
@media (max-width: 768px) {
  .board {
    grid-template-columns: minmax(0, 1fr);
  }

  .col + .col {
    border-left: none;
    border-top: 1px solid var(--border);
  }

  .picker-body {
    height: 300px;
  }

  .targets {
    max-height: 320px;
  }
}

/* Below the sheet breakpoint the inverted L flattens too: the rail becomes a
   chip strip under the tabs, because even 96px of side rail is width a phone
   does not have to give. */
@media (max-width: 640px) {
  .picker-body {
    grid-template-columns: minmax(0, 1fr);
    grid-template-rows: auto minmax(0, 1fr);
    height: 300px;
  }

  .rail {
    flex-direction: row;
    align-items: center;
    gap: var(--space-1);
    border-right: none;
    border-bottom: 1px solid var(--border);
    overflow-x: auto;
    overflow-y: hidden;
    scrollbar-width: none;
  }

  .rail::-webkit-scrollbar {
    display: none;
  }

  .rail-item {
    min-height: 34px;
    border-color: var(--border);
    border-radius: var(--radius-full);
    white-space: nowrap;
  }

  .rail-name,
  .rail-hint {
    overflow: visible;
  }
}

/* Touch targets: the select matches what a small button gets here, and 16px
   type so iOS Safari does not zoom the sheet when it takes focus. The chip's
   remove grows too — not to the full 40px floor, which would double the height
   of every chip, but to the same 28px the dialog's own close button uses. */
@media (pointer: coarse) {
  .select {
    min-height: 34px;
    font-size: var(--text-md);
  }

  .chip-remove {
    width: 28px;
    height: 28px;
  }
}
</style>
