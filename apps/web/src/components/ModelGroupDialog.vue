<script setup lang="ts">
/**
 * Create / edit a model group (docs/admin-ui.md § Groups page).
 *
 * A group is a bare name plus an **ordered** list of `provider/model` targets:
 * the order is the routing priority, which is why the list is built with move
 * controls rather than a set of checkboxes, and why saving always sends the
 * whole list.
 *
 * The picker filters the catalog the page already loaded — a keystroke never
 * hits the network — and free text is a first-class path beside it: the server
 * validates only a target's provider prefix, so an upstream id the catalog
 * doesn't list is legitimate.
 *
 * Delete lives in the footer, same reasoning as key revoke: rare and
 * irreversible, so it costs opening the group first and confirming.
 */
import { computed, ref } from "vue"
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
  type CatalogModel,
  type ModelGroup,
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

const name = ref(props.group?.name ?? "")
const targets = ref<string[]>([...(props.group?.targets ?? [])])
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
 * Catalog rows that are still addable. Group aliases are filtered out by their
 * own shape — a bare name has no "/" — because a group can never target
 * another group (docs/providers.md § Model groups).
 */
const suggestions = computed<CatalogModel[]>(() => {
  const q = trimmedQuery.value.toLowerCase()
  const chosen = new Set(targets.value)
  const out: CatalogModel[] = []
  for (const model of props.catalog) {
    if (!isTargetId(model.id) || chosen.has(model.id)) continue
    if (q && !model.id.toLowerCase().includes(q) && !model.display_name.toLowerCase().includes(q)) {
      continue
    }
    out.push(model)
    if (out.length === MAX_SUGGESTIONS) break
  }
  return out
})

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

function addTarget(target: string) {
  const value = target.trim()
  targetError.value = null
  if (!isTargetId(value)) {
    targetError.value = t("groups.error.targetFormat", { example: MODEL_ID_FORM })
    return
  }
  if (targets.value.includes(value)) {
    // Rejected here rather than at save: the list is what the user is reading,
    // so the moment to say "already in this group" is the moment they add it.
    targetError.value = t("groups.error.targetDuplicate")
    return
  }
  if (targets.value.length >= MODEL_GROUP_TARGETS_MAX) {
    targetError.value = t("groups.error.targetsMax", { max: MODEL_GROUP_TARGETS_MAX })
    return
  }
  targets.value.push(value)
  query.value = ""
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
    target: moved,
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
    const body = { name: name.value.trim(), targets: [...targets.value] }
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
          <li v-for="(target, index) in targets" :key="target" class="target">
            <span class="pos tabular">{{ index + 1 }}</span>
            <code class="mono target-id" :title="target">{{ target }}</code>
            <div class="target-actions">
              <AppButton
                icon-only
                size="sm"
                variant="ghost"
                :label="t('groups.dialog.moveUp', { target })"
                :disabled="index === 0 || saving || deleting"
                @click="move(index, index - 1)"
              >
                <template #icon><ActionIcon name="arrow-up" /></template>
              </AppButton>
              <AppButton
                icon-only
                size="sm"
                variant="ghost"
                :label="t('groups.dialog.moveDown', { target })"
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
                :label="t('groups.dialog.removeTarget', { target })"
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

.target {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  flex-wrap: wrap;
  padding: var(--space-2);
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
}

/* Sized to two digits — the list stops at 20 — so the ids line up in a column
   rather than shifting by one character at position 10. */
.pos {
  flex-shrink: 0;
  width: 16px;
  text-align: right;
  color: var(--faint);
  font-size: var(--text-2xs);
}

.target-id {
  flex: 1 1 120px;
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
  flex-shrink: 0;
  margin-left: auto;
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
</style>
