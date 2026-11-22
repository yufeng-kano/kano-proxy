<!--
  The one place table markup lives.

  Three things a page's own `<table>` reliably gets wrong, all handled here:

  1. Sticky header — the column names stay while the body scrolls inside its
     card, which is what lets a long table live in a bounded region instead of
     growing the page (docs/admin-ui.md § Anti-scroll rules).
  2. Alignment — numeric columns are centered with tabular numerals, header and
     cells together, declared once per column rather than re-specified per cell.
  3. Mobile — below 768px each row becomes a card of label/value pairs.
     Horizontal scroll on a phone hides exactly the columns that matter, so
     that is not the fallback.

  Cells render through a per-column slot named `cell-<key>`, falling back to
  the column's `value()`.

  `fixed` (below) adds a fourth, opt-in: a table whose columns are pixel tracks
  rather than sized by their content, with one-line cells, optional resize
  handles and optional group separator rows.
-->

<!-- A plain block, not a second `<script setup>`: only one setup block is
     compiled, and a type exported from a second one is silently dropped along
     with every macro in it. -->
<script lang="ts">
export type Column<Row> = {
  key: string
  header: string
  /** Centered under its header, with tabular numerals. */
  numeric?: boolean
  /**
   * Right-aligns the header *and* the cells without the numeric treatment —
   * for a control column (Copy, Edit). Alignment belongs to the column, not
   * to a wrapper each page re-invents around its own button.
   */
  align?: "end"
  /** Plain-text value; also the mobile card's value when no slot is given. */
  value?: (row: Row) => string
  /** Hidden on mobile cards — for columns that only add noise there. */
  hideOnMobile?: boolean
  /**
   * Track width. An action column needs one: without it the column takes the
   * table's leftover width and its header ends up at the far edge of a 400px
   * cell from the control it labels.
   *
   * Under `fixed` this is not a hint but the track itself, in px. At most one
   * column leaves it unset and gives `minWidth` instead: that one takes the
   * card's width beyond the others.
   */
  width?: string
  /** Under `fixed`, the column's floor in px: for the flexible column, and for resizing (48 without one). */
  minWidth?: number
  /**
   * Accessible name for a column whose `header` is intentionally blank — an
   * action column, where a visible word would label a control that already
   * carries its own. A blank `<th>` is an unnamed column to a screen reader
   * reading the table's structure, so the name is rendered visually hidden
   * rather than omitted.
   */
  srHeader?: string
}
</script>

<script setup lang="ts" generic="Row">
import { computed } from "vue"

const props = defineProps<{
  columns: Column<Row>[]
  rows: Row[]
  rowKey: (row: Row) => string
  /** Accessible caption. Visually hidden — the card header carries the visible title. */
  caption: string
  /**
   * Makes the whole row the pointer target for its primary action (Models'
   * click-to-copy). The row action stays a real focusable control — this only
   * widens the hit area, so keyboard and screen-reader users are unaffected.
   */
  rowClickable?: boolean
  /**
   * Pixel tracks and one-line cells, for a table with more columns than the
   * card has width (Logs, eleven of them).
   *
   * Auto layout sizes a column by its longest cell, so one long model id widens
   * the table and every other column moves with it. `table-layout: fixed`
   * makes each `width` the track itself; a cell that does not fit is cut with
   * an ellipsis, never wrapped into a taller row (docs/admin-ui.md § Logs page).
   * The table is never narrower than its tracks, so a card narrower than them
   * scrolls sideways.
   *
   * It lives here rather than in the page's scoped styles because the cut
   * needs a wrapper element inside each `<td>` — markup only this component
   * emits — and because the mobile card fallback has to opt back out of it in
   * the same place it is defined. Opt-in: the four- and five-column tables
   * size themselves fine, and cutting their cells would hide text that fits.
   *
   * A page that turns it on owns the tooltips: a cut cell should carry its
   * full text as a native `title`.
   */
  fixed?: boolean
  /**
   * Under `fixed`, each column's right edge is a resize handle, as in a
   * spreadsheet — a pointer drag, or ←/→ on the focused handle — that changes
   * that column's width alone: every column to its right moves over by the
   * same amount and keeps its own width. The edge can be grabbed from either
   * side of the line (a 12px target centred on it), so a column's left edge,
   * which is its neighbour's right edge, answers too. A column stops at its
   * `minWidth` (48px without one) and at 4000px, wider than any card, so
   * pinning the flexible column never cuts it.
   *
   * The drag emits `resize(key, px)`. A flexible column has no track to
   * resize, so the first drag also emits its rendered width, pinning it:
   * otherwise it would absorb the change and the dragged edge would stay put.
   * From then on every column is a track and the table is as wide as their
   * sum; what is left of the card is a blank filler column that carries the
   * header fill and row rules to the card's edge. The page owns the widths
   * and keeps them; clearing them brings the flexible column back.
   */
  resizable?: boolean
  /** The handle's accessible name for a column, e.g. "Resize Account". */
  resizeLabel?: (header: string) => string
  /**
   * Under `fixed`, a label per row: a full-width separator row goes in
   * wherever it changes (Logs' day headings).
   */
  groupBy?: (row: Row) => string
  /**
   * Drops the visible column-header row, for a table whose columns are
   * self-evident *and* whose card head already names the collection (Models'
   * provider cards). Repeated once per card down a stack, MODEL ID / NAME /
   * COPY is three words restated above a monospaced id and a plain name
   * (docs/admin-ui.md § Design restraint).
   *
   * The `<th>`s stay in the markup with their labels visually hidden, so the
   * table is still a named structure to a screen reader and each `<td>` keeps
   * the `data-label` the mobile card fallback prints. They collapse to zero
   * height rather than being pulled out of flow: an absolutely positioned
   * `<th>` leaves the table's column set, taking its `width` with it, and the
   * tracks a page declared (52% / 72px) would silently stop applying.
   *
   * Opt-in — a table that is a page's whole content has no card head naming
   * its columns, so it keeps its headers.
   */
  hideHeader?: boolean
}>()

const emit = defineEmits<{ rowClick: [Row]; resize: [key: string, width: number] }>()

defineSlots<Record<string, (props: { row: Row }) => unknown>>()

const MIN_WIDTH = 48
const MAX_WIDTH = 4000
const KEY_STEP = 16

/** Each row with the group heading that goes above it, if any. */
const entries = computed(() => {
  let previous: string | null = null
  return props.rows.map((row) => {
    const label = props.groupBy?.(row) ?? null
    const group = label !== null && label !== previous ? label : null
    previous = label
    return { row, group }
  })
})

/** Index of the one column without a track, or -1. */
const flexIndex = computed(() => (props.fixed ? props.columns.findIndex((column) => !column.width) : -1))

/* With every column a track the table is their sum, and a blank last column
   takes the rest of the card so the header fill and row rules reach its edge
   (see `resizable`). */
const filler = computed(() => props.fixed && flexIndex.value === -1)

/**
 * The narrowest the fixed table may get: every pixel track plus the flexible
 * column's floor. It is still `width: 100%`, so a wider card goes to the
 * flexible column, or to the filler when there is none.
 */
const fixedStyle = computed(() => {
  if (!props.fixed) return undefined
  let total = 0
  for (const column of props.columns) total += column.width ? Number.parseFloat(column.width) : (column.minWidth ?? 0)
  return { minWidth: `${total}px` }
})

/* One gesture on column `index`'s right edge: its width at the start, then
   the pointer's travel, clamped to the column's floor and ceiling. */
let drag: { key: string; width: number; floor: number; x: number } | null = null

function begin(event: Event, index: number): { key: string; width: number; floor: number } | null {
  const column = props.columns[index]
  const header = (event.currentTarget as HTMLElement).closest("tr")?.children[index]
  if (!column || !(header instanceof HTMLElement)) return null
  // Pin the flexible column where it is before anything moves (see `resizable`).
  const flex = props.columns[flexIndex.value]
  const flexHeader = header.parentElement?.children[flexIndex.value]
  if (flex && flexHeader) {
    // Floor, not round: a pin a fraction wider than the card would overflow it.
    emit("resize", flex.key, Math.min(MAX_WIDTH, Math.floor(flexHeader.getBoundingClientRect().width)))
  }
  return {
    key: column.key,
    // A track's own value: zoom makes the rendered width fractional.
    width: column.width ? Number.parseFloat(column.width) : Math.round(header.getBoundingClientRect().width),
    floor: column.minWidth ?? MIN_WIDTH,
  }
}

function move(gesture: { key: string; width: number; floor: number }, travel: number) {
  emit("resize", gesture.key, Math.min(MAX_WIDTH, Math.max(gesture.floor, Math.round(gesture.width + travel))))
}

function onResizeStart(event: PointerEvent, index: number) {
  const gesture = begin(event, index)
  if (!gesture) return
  event.preventDefault()
  ;(event.currentTarget as HTMLElement).setPointerCapture(event.pointerId)
  drag = { ...gesture, x: event.clientX }
}

function onResizeMove(event: PointerEvent) {
  if (drag) move(drag, event.clientX - drag.x)
}

function onResizeEnd() {
  drag = null
}

function onResizeKey(event: KeyboardEvent, index: number) {
  if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return
  event.preventDefault()
  const gesture = begin(event, index)
  if (gesture) move(gesture, event.key === "ArrowRight" ? KEY_STEP : -KEY_STEP)
}
</script>

<template>
  <div class="table-wrap">
    <table class="table" :class="{ fixed, headerless: hideHeader }" :style="fixedStyle">
      <caption class="sr-only">{{ caption }}</caption>
      <thead>
        <tr>
          <th
            v-for="(column, index) in columns"
            :key="column.key"
            scope="col"
            :class="{ numeric: column.numeric, end: column.align === 'end' }"
            :style="column.width ? { width: column.width } : undefined"
          >
            <span
              v-if="hideHeader || (!column.header && column.srHeader)"
              class="sr-only"
            >{{ column.header || column.srHeader }}</span>
            <template v-else>{{ column.header }}</template>
            <span
              v-if="fixed && resizable"
              class="resize"
              role="separator"
              aria-orientation="vertical"
              tabindex="0"
              :aria-label="resizeLabel?.(column.header) ?? column.header"
              @pointerdown="onResizeStart($event, index)"
              @pointermove="onResizeMove"
              @pointerup="onResizeEnd"
              @pointercancel="onResizeEnd"
              @keydown="onResizeKey($event, index)"
            />
            <!-- The previous column's edge, grabbed from this side of the line.
                 Pointer only: the keyboard reaches each edge once, above. -->
            <span
              v-if="fixed && resizable && index > 0"
              class="resize start"
              aria-hidden="true"
              @pointerdown="onResizeStart($event, index - 1)"
              @pointermove="onResizeMove"
              @pointerup="onResizeEnd"
              @pointercancel="onResizeEnd"
            />
          </th>
          <th v-if="filler" class="filler" aria-hidden="true">
            <span
              v-if="resizable"
              class="resize start"
              @pointerdown="onResizeStart($event, columns.length - 1)"
              @pointermove="onResizeMove"
              @pointerup="onResizeEnd"
              @pointercancel="onResizeEnd"
            />
          </th>
        </tr>
      </thead>
      <tbody>
        <template v-for="{ row, group } in entries" :key="rowKey(row)">
          <tr v-if="group !== null" class="group-row">
            <th scope="colgroup" :colspan="columns.length + (filler ? 1 : 0)">{{ group }}</th>
          </tr>
          <tr
            :class="{ clickable: rowClickable }"
            @click="rowClickable && $emit('rowClick', row)"
          >
            <td
              v-for="column in columns"
              :key="column.key"
              :class="{
                numeric: column.numeric,
                end: column.align === 'end',
                'hide-mobile': column.hideOnMobile,
              }"
              :data-label="column.header || column.srHeader"
            >
              <!-- The ellipsis needs a block box with `overflow: hidden`, and a
                   `<td>` that is not `display: table-cell` stops being a column —
                   so the cut goes on a wrapper inside the cell. Only under
                   `fixed`: wrapping every table's cells in a block would re-flow
                   the inline content of four other pages to buy them nothing. -->
              <div v-if="fixed" class="cell">
                <slot :name="`cell-${column.key}`" :row="row">
                  {{ column.value?.(row) ?? "" }}
                </slot>
              </div>
              <slot v-else :name="`cell-${column.key}`" :row="row">
                {{ column.value?.(row) ?? "" }}
              </slot>
            </td>
            <td v-if="filler" class="filler" aria-hidden="true" />
          </tr>
        </template>
      </tbody>
    </table>
  </div>
</template>

<style scoped>
.table-wrap {
  min-width: 0;
}

.table {
  width: 100%;
  border-collapse: separate;
  border-spacing: 0;
  font-size: var(--text-sm);
}

.table th {
  position: sticky;
  top: 0;
  z-index: 1;
  height: 36px;
  padding: 0 var(--space-4);
  text-align: left;
  font-size: var(--text-2xs);
  font-weight: var(--weight-semibold);
  text-transform: uppercase;
  letter-spacing: var(--tracking-wide);
  color: var(--muted);
  white-space: nowrap;
  /* Opaque, not translucent: rows scrolling under a semi-transparent header
     smear into the labels. */
  background: var(--surface-2);
  /* An inset shadow, not `border-bottom`: a sticky cell's border scrolls away
     independently of the cell itself, leaving the header floating unruled. */
  box-shadow: inset 0 -1px 0 var(--border);
}

.table td {
  padding: var(--space-3) var(--space-4);
  border-bottom: 1px solid var(--border);
  vertical-align: middle;
  color: var(--text-secondary);
}

.table tbody tr:last-child td {
  border-bottom: none;
}

.table tbody tr:hover td {
  background: var(--surface-2);
}

.table tbody tr.clickable {
  cursor: pointer;
}

/*
 * Column alignment, applied to the header and its cells as one.
 *
 * These are written as `.table th`/`.table td` rather than bare `.numeric`
 * because `.table th` sets `text-align: left` at specificity (0,1,1) and a bare
 * `.numeric` is (0,1,0) — the header won, so every numeric column shipped a
 * left-aligned header over right-aligned figures, and the two never lined up.
 *
 * Numbers are centered on the column rather than pushed to its edge: a header
 * like SPEND or MIN is far narrower than the track it names, and right-aligning
 * the figures strands them a column's width away from the word. Tabular
 * numerals still make the digits a fixed pitch, so the values remain a scannable
 * block; the decimal points align exactly when the values are the same width.
 */
.table th.numeric,
.table td.numeric {
  text-align: center;
  font-variant-numeric: tabular-nums;
}

/* A control column: the header sits over the control rather than at the far
   edge of the track. */
.table th.end,
.table td.end {
  text-align: right;
}

/*
 * --- No visible header (the `hideHeader` prop) -----------------------------
 *
 * The cells keep their place in the table — they are still what sizes the
 * columns — and lose everything that draws them. `line-height: 0` because the
 * only child left is the `.sr-only` label, which is out of flow: without it a
 * stray line box would give the row a few pixels of height anyway. Nothing is
 * sticky, so the rule and the opaque fill that let the header survive scrolling
 * rows both go with it.
 */
.table.headerless th {
  position: static;
  height: 0;
  padding: 0;
  line-height: 0;
  background: none;
  box-shadow: none;
}

/*
 * --- Fixed tracks (the `fixed` prop) --------------------------------------
 *
 * Every column is its own pixel track; the one without a width takes the rest
 * of the card. Widths include the padding, so a dragged edge lands where the
 * pointer is.
 */
.table.fixed {
  table-layout: fixed;
}

.table.fixed th,
.table.fixed td {
  box-sizing: border-box;
}

/* Eleven tracks: the default `--space-4` gutter would spend a third of the
   table's width on padding, and every column would pay for it in text. A
   two-word figure header ("Cache write") may take two lines of the 36px row
   rather than force its column wider than the figures under it. */
.table.fixed th {
  padding: 0 var(--space-2);
  white-space: normal;
  line-height: 1.2;
}

.table.fixed td {
  padding: var(--space-2);
}

/* Figures line up on their last digit in a narrow track, header included. */
.table.fixed th.numeric,
.table.fixed td.numeric {
  text-align: right;
}

/* One line, cut with an ellipsis; the page puts the full text on a `title`. */
.table.fixed .cell {
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
  line-height: 22px;
}

/* The resize handle: a 6px strip inside the header's right edge (inside, so
   the next sticky header cannot paint over it), a rule only while pointed at
   or focused, so the header row reads as plain labels. `.start` below is the
   same edge's strip on the next header's side. */
.resize {
  position: absolute;
  top: 0;
  right: 0;
  z-index: 2;
  width: 6px;
  height: 100%;
  cursor: col-resize;
  touch-action: none;
}

.resize::after {
  content: "";
  position: absolute;
  top: 8px;
  bottom: 8px;
  right: 0;
  width: 2px;
  border-radius: 1px;
  background: transparent;
  transition: background var(--duration-fast) var(--ease);
}

/* The other half of an edge's hit area, inside the next header's left edge.
   Together the two make a 12px target centred on the line. */
.resize.start {
  right: auto;
  left: 0;
}

/* One line per edge, wherever the pointer is on it: the rule belongs to the
   left half, and lights from either half. */
.resize.start::after {
  display: none;
}

.table th:has(+ th > .resize.start:hover) > .resize:not(.start)::after {
  background: var(--ring-border);
}

/* Blank: it only carries the header fill and the row rules to the card's
   edge, and takes no width the tracks need. */
.table.fixed th.filler,
.table.fixed td.filler {
  padding: 0;
}

.resize:hover::after,
.resize:focus-visible::after {
  background: var(--ring-border);
}

.resize:focus-visible {
  outline: none;
}

/* A day heading between rows: part of the table, so it scrolls with the rows
   it names, set like a section label rather than a column header. */
.table .group-row th {
  position: static;
  height: auto;
  padding: var(--space-4) var(--space-2) var(--space-1);
  font-size: var(--text-xs);
  font-weight: var(--weight-semibold);
  text-transform: none;
  letter-spacing: normal;
  color: var(--text-secondary);
  background: var(--surface);
  box-shadow: inset 0 -1px 0 var(--border);
}

/* Mobile: one card per row, each cell labelled by its column header. */
@media (max-width: 768px) {
  .table,
  .table tbody,
  .table tr,
  .table td {
    display: block;
    width: auto;
  }

  .table thead {
    display: none;
  }

  .table tbody tr {
    padding: var(--space-3) var(--space-4);
    border-bottom: 1px solid var(--border);
  }

  .table tbody tr:last-child {
    border-bottom: none;
  }

  .table td {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--space-4);
    padding: var(--space-1) 0;
    border-bottom: none;
    text-align: left;
  }

  .table tbody tr:hover td {
    background: transparent;
  }

  .table td::before {
    content: attr(data-label);
    flex-shrink: 0;
    font-size: var(--text-2xs);
    font-weight: var(--weight-medium);
    text-transform: uppercase;
    letter-spacing: var(--tracking-wide);
    color: var(--faint);
  }

  /* The first cell is the row's identity — it reads as a heading, not as a
     labelled field. */
  .table td:first-child {
    display: block;
    margin-bottom: var(--space-2);
    font-weight: var(--weight-medium);
    color: var(--text);
  }

  .table td:first-child::before {
    display: none;
  }

  .table td.numeric {
    text-align: right;
  }

  /* The row is a card here, so an action reads as its last field rather than
     something pinned to a column edge that no longer exists. */
  .table td.end {
    text-align: left;
  }

  .table td.hide-mobile,
  .table .filler {
    display: none;
  }

  /*
   * `fixed` is a desktop-table concern and stops here. There are no tracks left
   * to protect once a row is a card, and a card's value is the whole value —
   * clamping it would cut the very text the card fallback exists to show.
   *
   * The padding needs saying again: `.table.fixed td` outranks the `.table td`
   * rule above, so without this the card rows would keep the table's gutter.
   */
  .table.fixed td {
    padding: var(--space-1) 0;
  }

  .table.fixed .cell {
    min-width: 0;
    line-height: inherit;
    overflow: visible;
    white-space: normal;
    overflow-wrap: anywhere;
  }

  /* Tracks belong to the desktop table; a card is as wide as the screen. */
  .table.fixed {
    width: auto !important;
    min-width: 0 !important;
  }

  .table .group-row {
    padding: 0;
    border-bottom: 1px solid var(--border);
  }

  .table .group-row th {
    display: block;
    padding: var(--space-4) var(--space-4) var(--space-2);
  }
}
</style>
