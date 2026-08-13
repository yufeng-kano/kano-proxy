<script setup lang="ts">
/**
 * Create / edit a model group (docs/admin-ui.md § Groups page).
 *
 * A group is a bare name plus an **ordered** list of targets, each a
 * `provider/model` and the account it runs on: the order is the routing
 * priority, which is why the list is built with move controls rather than a
 * set of checkboxes, and why saving always sends the whole list.
 *
 * A target's account is either the provider's whole pool ("Any account", the
 * default) or one pinned account — pinning is what lets two accounts of the
 * same provider be ordered as two targets, so a model may legitimately appear
 * twice as long as the accounts differ. Duplicate identity is therefore
 * model + account, exactly as the server checks it.
 *
 * The picker filters the catalog the page already loaded — a keystroke never
 * hits the network — and free text is a first-class path beside it: the server
 * validates only a target's provider prefix, so an upstream id the catalog
 * doesn't list is legitimate. Account lists come from the same per-provider
 * endpoint the Providers page reads, loaded lazily for the providers this
 * group actually targets and cached by that composable.
 *
 * Delete lives in the footer, same reasoning as key revoke: rare and
 * irreversible, so it costs opening the group first and confirming.
 */
import { computed, ref, watch } from "vue"
import { useAccounts } from "@/composables/useAccounts"
import { useAuth } from "@/composables/useAuth"
import { useI18n } from "@/i18n"
import {
  ApiError,
  createModelGroup,
  deleteModelGroup,
  updateModelGroup,
} from "@/services/api"
import {
  MODEL_GROUP_NAME_MAX,
  MODEL_GROUP_TARGETS_MAX,
  PROVIDER_IDS,
  type CatalogModel,
  type ModelGroup,
  type ModelGroupTarget,
  type ProviderAccount,
  type ProviderId,
} from "@/types"
import ActionIcon from "./ui/ActionIcon.vue"
import AppButton from "./ui/AppButton.vue"
import Banner from "./ui/Banner.vue"
import FormField from "./ui/FormField.vue"
import Modal from "./ui/Modal.vue"
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

/**
 * Wire form, not copy: the shape of a target id is protocol (docs/api.md), so
 * it reads the same in every locale — same call as the `slug/*` preview in the
 * custom endpoint dialog.
 */
const MODEL_ID_FORM = "provider/model"

/**
 * How many matches the picker renders. The list is bounded and scrolls, so
 * this is only a ceiling on how much a catalog of thousands can cost to draw —
 * the search is how anything past it is reached.
 */
const MAX_SUGGESTIONS = 24

const isEdit = computed(() => !!props.group)

/**
 * A row carries a `uid` the group itself never has: the list is keyed by it so
 * re-pinning a row edits that row instead of replacing it. Keying by the
 * target's own identity would tear the row down on every account change — and
 * with it the select the user just used, focus included.
 */
type TargetRow = ModelGroupTarget & { uid: number }

let nextUid = 0

function toRow(target: ModelGroupTarget): TargetRow {
  return { ...target, uid: nextUid++ }
}

const name = ref(props.group?.name ?? "")
/** A working copy: the list is only sent on Save, never patched entry by entry. */
const targets = ref<TargetRow[]>((props.group?.targets ?? []).map(toRow))
const query = ref("")

const saving = ref(false)
const deleting = ref(false)
const nameError = ref<string | null>(null)
const targetError = ref<string | null>(null)
const error = ref<string | null>(null)
/** The move buttons' only feedback for a screen-reader user. */
const announcement = ref("")

/**
 * A target's shape, mirroring the server's `splitModelId`: a prefix and an
 * upstream id either side of the first "/", both non-empty. The upstream half
 * may itself contain further slashes.
 */
function isTargetId(value: string): boolean {
  const slash = value.indexOf("/")
  return slash > 0 && slash < value.length - 1
}

const trimmedQuery = computed(() => query.value.trim())

/**
 * Catalog rows the picker offers. Group aliases are filtered out by their own
 * shape — a bare name has no "/" — because a group can never target another
 * group (docs/providers.md § Model groups).
 *
 * Models already in the list are deliberately **not** filtered out: the same
 * model on two different accounts is two legitimate targets, and hiding it
 * after the first add would make the page's whole point unreachable.
 */
const suggestions = computed<CatalogModel[]>(() => {
  const q = trimmedQuery.value.toLowerCase()
  const out: CatalogModel[] = []
  for (const model of props.catalog) {
    if (!isTargetId(model.id)) continue
    if (q && !model.id.toLowerCase().includes(q) && !model.display_name.toLowerCase().includes(q)) {
      continue
    }
    out.push(model)
    if (out.length === MAX_SUGGESTIONS) break
  }
  return out
})

/** Text before the first "/" — a builtin provider id, or a custom slug. */
function prefixOf(model: string): string {
  const slash = model.indexOf("/")
  return slash === -1 ? model : model.slice(0, slash)
}

/**
 * The builtin provider a target belongs to, or null. Only builtins have an
 * accounts endpoint to list: a custom endpoint's key is one account the API
 * never exposes an id for, so those targets can only run on the whole pool —
 * which for a single-key endpoint is that key anyway.
 */
function builtinProviderOf(model: string): ProviderId | null {
  const prefix = prefixOf(model)
  return PROVIDER_IDS.includes(prefix as ProviderId) ? (prefix as ProviderId) : null
}

/** The bound accounts a target may pin, in the pool's own order. */
function poolFor(model: string): ProviderAccount[] {
  const provider = builtinProviderOf(model)
  if (!provider) return []
  return accounts.byProvider[provider].data?.accounts ?? []
}

/** The user's own name wins, then the upstream identity, then a short id. */
function accountName(account: ProviderAccount): string {
  return account.custom_label || account.label || account.id.slice(0, 8)
}

/**
 * What a pinned target's account is called right now. The loaded pool wins
 * over `account_label`: the label is what the *last read* resolved, and the
 * pool is live — it also names an account the server had no label for at all,
 * which would otherwise read as removed. `null` only when nothing knows the
 * account any more.
 */
function pinnedLabel(target: ModelGroupTarget): string | null {
  if (!target.account_id) return null
  const inPool = poolFor(target.model).find((a) => a.id === target.account_id)
  return inPool ? accountName(inPool) : target.account_label
}

type AccountOption = { value: string; label: string; disabled: boolean }

/**
 * "Any account" first — the default and the common case — then the provider's
 * bound accounts. A pin the pool no longer offers is appended so the select
 * never silently drops it: labelled by whatever is still known, or as removed,
 * in which case it is disabled so the only way out is forward.
 */
function accountOptions(target: ModelGroupTarget): AccountOption[] {
  const options: AccountOption[] = [
    { value: "", label: t("groups.account.any"), disabled: false },
  ]
  for (const account of poolFor(target.model)) {
    options.push({ value: account.id, label: accountName(account), disabled: false })
  }
  if (target.account_id && !options.some((o) => o.value === target.account_id)) {
    const known = pinnedLabel(target)
    options.push({
      value: target.account_id,
      label: known ?? t("groups.account.missing"),
      disabled: !known,
    })
  }
  return options
}

/**
 * A pin whose account is gone: neither the provider's pool nor the server's
 * read-time label knows it any more (docs/auth.md § Model groups). The row
 * stays, warned and re-pickable — dropping the pin silently would change what
 * the group routes to without saying so.
 */
function isMissingAccount(target: ModelGroupTarget): boolean {
  return !!target.account_id && !pinnedLabel(target)
}

/**
 * Identity the server dedupes on: model *and* account together. The separator
 * is a NUL — never legal inside a model id or an account id — written as an
 * escape so this file stays plain text, exactly as the server writes it.
 */
function identityOf(target: ModelGroupTarget): string {
  return `${target.model}\u0000${target.account_id ?? ""}`
}

/**
 * Accounts are fetched only for the providers this group actually targets, and
 * only once each — the composable is cache-first, so a recent visit to
 * Providers costs nothing here.
 */
const targetedProviders = computed(() => {
  const out = new Set<ProviderId>()
  for (const target of targets.value) {
    const provider = builtinProviderOf(target.model)
    if (provider) out.add(provider)
  }
  return [...out]
})

const requested = new Set<ProviderId>()

watch(
  targetedProviders,
  (providers) => {
    accounts.setUserId(user.value?.id ?? null)
    for (const provider of providers) {
      if (requested.has(provider)) continue
      requested.add(provider)
      void accounts.loadProvider(provider)
    }
  },
  { immediate: true },
)

/**
 * What the Add button (and Enter in the field) adds: the typed id when it is a
 * full one the catalog does not list, otherwise the top match. Nothing at all
 * while the field is empty — the list below is how the catalog is browsed.
 */
const addCandidate = computed(() => {
  const raw = trimmedQuery.value
  if (!raw) return null
  if (isTargetId(raw) && !suggestions.value.some((m) => m.id === raw)) return raw
  return suggestions.value[0]?.id ?? null
})

const canSave = computed(() => targets.value.length > 0)

/** Added unpinned; the row's own select is where an account is chosen. */
function addTarget(model: string) {
  const value = model.trim()
  targetError.value = null
  if (!isTargetId(value)) {
    targetError.value = t("groups.error.targetFormat", { example: MODEL_ID_FORM })
    return
  }
  const target: ModelGroupTarget = { model: value, account_id: null, account_label: null }
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
  query.value = ""
}

/**
 * Re-pinning in place, rather than remove-and-re-add: changing an account is
 * the common edit once a group exists. A change that would collide with
 * another row is refused and the row keeps what it had — the same rule the
 * server would apply on Save, applied where the user can see it. Returns
 * whether the change was taken.
 */
function setAccount(index: number, accountId: string): boolean {
  const target = targets.value[index]
  if (!target) return false
  const next = accountId || null
  if (next === target.account_id) return true

  const candidate: ModelGroupTarget = { ...target, account_id: next }
  if (targets.value.some((t, i) => i !== index && identityOf(t) === identityOf(candidate))) {
    targetError.value = t("groups.error.targetDuplicate")
    return false
  }

  targetError.value = null
  target.account_id = next
  // The label is display data the server resolves; keep it in step locally so
  // the row stops reading as a removed account the moment it is re-pinned.
  const picked = poolFor(target.model).find((a) => a.id === next)
  target.account_label = picked ? accountName(picked) : null
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
 * The provider's accounts are still on their way. The select is held disabled
 * for that moment rather than offering "Any account" alone, which would read as
 * "this provider has nothing to pin". A *failed* load is not pending: the list
 * stays enabled so an existing pin can still be cleared.
 */
function accountsPending(model: string): boolean {
  const provider = builtinProviderOf(model)
  if (!provider) return false
  const state = accounts.byProvider[provider]
  return !state.data && !state.error
}

function addFromQuery() {
  if (!addCandidate.value) return
  addTarget(addCandidate.value)
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

/** Mirrors the server rule so a violation never costs a round trip. */
function validateName(): string | null {
  const value = name.value.trim()
  if (!value) return t("groups.error.name")
  if (value.length > MODEL_GROUP_NAME_MAX) {
    return t("groups.error.nameLength", { max: MODEL_GROUP_NAME_MAX })
  }
  if (/\s/.test(value)) return t("groups.error.nameWhitespace")
  if (value.includes("/")) return t("groups.error.nameSlash")
  return null
}

/**
 * A rejected write answers with a single `error` string rather than a field map
 * (docs/auth.md § Model groups), so the message is placed by what it is about:
 * anything naming the group's name lands on the name field, anything naming a
 * target lands under the list, and everything else is a banner.
 */
function applyServerError(e: unknown, fallback: string) {
  const message = e instanceof ApiError && e.status === 400 ? e.message : null
  if (!message) {
    error.value = fallback
    return
  }
  if (/^name\b|already exists/i.test(message)) nameError.value = message
  else if (/^targets?\b|^duplicate target/i.test(message)) targetError.value = message
  else error.value = message
}

async function submit() {
  if (saving.value || !canSave.value) return
  nameError.value = null
  targetError.value = null
  error.value = null

  const invalidName = validateName()
  if (invalidName) {
    nameError.value = invalidName
    return
  }

  saving.value = true
  try {
    const body = {
      name: name.value.trim(),
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
    size="md"
    :title="isEdit ? t('groups.dialog.editTitle') : t('groups.create')"
    @close="emit('close')"
  >
    <div class="body">
      <FormField
        v-slot="field"
        :label="t('groups.dialog.nameLabel')"
        :hint="t('groups.dialog.nameHint')"
        :error="nameError ?? undefined"
      >
        <TextInput
          :id="field.id"
          v-model="name"
          mono
          :placeholder="t('groups.dialog.namePlaceholder')"
          :described-by="field.describedBy"
          :invalid="field.invalid"
          :disabled="saving || deleting"
          @enter="submit"
        />
      </FormField>

      <!-- A real fieldset/legend so the group's name reaches assistive tech
           natively, matching the endpoint dialog's models block. -->
      <fieldset class="fieldset">
        <legend class="field-label">{{ t("groups.dialog.targetsLabel") }}</legend>
        <p class="field-hint">{{ t("groups.dialog.targetsHint") }}</p>

        <!-- The position is the routing rule, so it is real text in the row
             rather than a list marker: `list-style: none` drops list semantics
             in Safari, and the number is the one thing that must survive. -->
        <ol v-if="targets.length" class="targets">
          <li v-for="(target, index) in targets" :key="target.uid" class="target">
            <span class="pos tabular">{{ index + 1 }}</span>
            <code class="mono target-id" :title="target.model">{{ target.model }}</code>

            <!-- Per-target facts sit in their own line under the id: the
                 account today, weight and live usage when balancing lands. -->
            <div class="target-facts">
              <label class="sr-only" :for="`target-account-${target.uid}`">
                {{ t("groups.dialog.accountLabel", { target: target.model }) }}
              </label>
              <select
                :id="`target-account-${target.uid}`"
                class="select"
                :class="{ invalid: isMissingAccount(target) }"
                :value="target.account_id ?? ''"
                :disabled="saving || deleting || accountsPending(target.model)"
                @change="onAccountChange(index, $event)"
              >
                <option
                  v-for="option in accountOptions(target)"
                  :key="option.value"
                  :value="option.value"
                  :disabled="option.disabled"
                >
                  {{ option.label }}
                </option>
              </select>
              <span v-if="isMissingAccount(target)" class="target-warning">
                {{ t("groups.account.skipped") }}
              </span>
            </div>

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
          </li>
        </ol>
        <p v-else class="field-hint">{{ t("groups.dialog.targetsEmpty") }}</p>

        <p v-if="targetError" class="field-error" role="alert">{{ targetError }}</p>
      </fieldset>

      <FormField
        v-slot="field"
        :label="t('groups.dialog.addLabel')"
        :hint="t('groups.dialog.addHint')"
      >
        <div class="add">
          <TextInput
            :id="field.id"
            v-model="query"
            type="search"
            mono
            :placeholder="t('groups.dialog.addPlaceholder')"
            :described-by="field.describedBy"
            :disabled="saving || deleting"
            @enter="addFromQuery"
          />
          <AppButton
            :label="addCandidate ? t('groups.dialog.addTarget', { target: addCandidate }) : undefined"
            :disabled="!addCandidate || saving || deleting"
            @click="addFromQuery"
          >
            {{ t("groups.dialog.add") }}
          </AppButton>
        </div>
      </FormField>

      <!-- Filtered client-side over the catalog the page already holds, so a
           keystroke costs nothing. -->
      <ul v-if="suggestions.length" class="suggestions">
        <li v-for="model in suggestions" :key="model.id">
          <AppButton
            size="sm"
            variant="ghost"
            class="suggestion"
            :label="t('groups.dialog.addTarget', { target: model.id })"
            :disabled="saving || deleting"
            @click="addTarget(model.id)"
          >
            <code class="mono suggestion-id">{{ model.id }}</code>
            <span class="suggestion-name">{{ model.display_name }}</span>
          </AppButton>
        </li>
      </ul>
      <p v-else-if="trimmedQuery" class="field-hint">
        {{ t("groups.dialog.noMatches", { query: trimmedQuery }) }}
      </p>

      <Banner v-if="error" tone="error">{{ error }}</Banner>

      <!-- Outside the list above so it survives its rerender. -->
      <span class="sr-only" role="status" aria-live="polite">{{ announcement }}</span>
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
.body {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
}

/* A fieldset's default margin, padding, and border are browser chrome this
   design does not use — the legend alone carries the grouping. */
.fieldset {
  display: grid;
  gap: var(--space-2);
  margin: 0;
  padding: 0;
  border: none;
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

/* --- Ordered targets ----------------------------------------------------- */

.targets {
  display: grid;
  gap: var(--space-1);
  margin: 0;
  padding: 0;
  list-style: none;
}

/* Three tracks — position, subject, controls — and a second line under the
   subject for the row's facts. The facts line is where per-target balancing
   lands later (weight, live usage) beside the account, so growing it costs a
   chip rather than a new row shape. */
.target {
  display: grid;
  grid-template-columns: 16px minmax(0, 1fr) auto;
  align-items: center;
  gap: var(--space-1) var(--space-2);
  padding: var(--space-2);
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
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

.target-facts {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  flex-wrap: wrap;
  grid-column: 2 / -1;
  min-width: 0;
}

.target-actions {
  display: flex;
  align-items: center;
  gap: var(--space-1);
  grid-column: 3;
  grid-row: 1;
}

/* TextInput's control spec at the small size, so the account select sits level
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

/* A pin whose account is gone: the border carries the tone, the sentence
   beside it carries the meaning — color is never the only signal. */
.select.invalid {
  border-color: var(--warn-border);
  color: var(--warn);
}

.target-warning {
  color: var(--warn);
  font-size: var(--text-2xs);
  overflow-wrap: anywhere;
}

/* --- Picker -------------------------------------------------------------- */

.add {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  width: 100%;
}

/* Bounded so a long catalog cannot push Save out of reach; the search is how
   the rest of it is reached. */
.suggestions {
  display: grid;
  gap: 2px;
  margin: 0;
  padding: 0;
  max-height: 168px;
  overflow-y: auto;
  overscroll-behavior: contain;
  list-style: none;
}

/* A full-width row rather than a pill: the id is what the user is reading down
   the list, so the button is shaped to it. Selected through the list so these
   outrank AppButton's own single-class rules rather than depending on style
   order. */
.suggestions :deep(.suggestion) {
  width: 100%;
  justify-content: flex-start;
}

/* The default slot lands in one flex item, so the id/name split is set up
   inside it. */
.suggestions :deep(.suggestion .btn-label) {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: var(--space-3);
  width: 100%;
  min-width: 0;
  font-weight: var(--weight-normal);
}

.suggestion-id {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.suggestion-name {
  flex-shrink: 0;
  color: var(--muted);
  font-size: var(--text-2xs);
}

/* The footer packs its buttons to the right; the auto margin is what separates
   Delete from Cancel/Save rather than a gap nobody else in the app has. */
.delete {
  margin-right: auto;
}

/* A small button's touch height here, and 16px type so iOS Safari does not zoom
   the dialog when the select takes focus. */
@media (pointer: coarse) {
  .select {
    min-height: 34px;
    font-size: var(--text-md);
  }
}
</style>
