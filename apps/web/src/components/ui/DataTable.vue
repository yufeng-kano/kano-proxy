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

  `fixed` (below) adds a fourth, opt-in: a table whose columns are shares of the
  card rather than of their own content.
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
  align?: "start" | "end"
  /** Plain-text value; also the mobile card's value when no slot is given. */
  value?: (row: Row) => string
  /** Hidden on mobile cards — for columns that only add noise there. */
  hideOnMobile?: boolean
  /**
   * Track width. An action column needs one: without it the column takes the
   * table's leftover width and its header ends up at the far edge of a 400px
   * cell from the control it labels.
   *
   * Under `fixed` this is not a hint but the track itself, so there give every
   * column a percentage and make them sum to 100 — a column left without one
   * splits whatever the others did not claim.
   */
  width?: string
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
defineProps<{
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
   * Fixed tracks and two-line cells, for a table with more columns than the
   * card has width (Logs, eleven of them).
   *
   * Auto layout sizes a column by its longest cell, so one long model id widens
   * the table past its card and the card grows a horizontal scrollbar that
   * parks the last columns out of sight. `table-layout: fixed` makes each
   * `width` a share of the card that no content can argue with; every cell is
   * then clamped to the row's two lines and cut, never wrapped into a taller
   * row (docs/admin-ui.md § Logs page).
   *
   * It lives here rather than in the page's scoped styles because the clamp
   * needs a wrapper element inside each `<td>` — markup only this component
   * emits — and because the mobile card fallback has to opt back out of it in
   * the same place it is defined. It stays opt-in because it is a real
   * constraint on the content: the four- and five-column tables size themselves
   * fine, and clamping their cells would cut text that fits today.
   *
   * A page that turns it on owns the tooltips: a clamped cell should carry its
   * full text as a native `title`, since what the clamp cut is otherwise only
   * in the row detail.
   */
  fixed?: boolean
}>()

defineEmits<{ rowClick: [Row] }>()

defineSlots<Record<string, (props: { row: Row }) => unknown>>()
</script>

<template>
  <div class="table-wrap">
    <table class="table" :class="{ fixed }">
      <caption class="sr-only">{{ caption }}</caption>
      <thead>
        <tr>
          <th
            v-for="column in columns"
            :key="column.key"
            scope="col"
            :class="{ numeric: column.numeric, end: column.align === 'end' }"
            :style="column.width ? { width: column.width } : undefined"
          >
            <span v-if="!column.header && column.srHeader" class="sr-only">
              {{ column.srHeader }}
            </span>
            <template v-else>{{ column.header }}</template>
          </th>
        </tr>
      </thead>
      <tbody>
        <tr
          v-for="row in rows"
          :key="rowKey(row)"
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
            <!-- The line clamp needs a `display: -webkit-box` box, and a `<td>`
                 that is not `display: table-cell` stops being a column — so the
                 clamp goes on a wrapper inside the cell. Only under `fixed`:
                 wrapping every table's cells in a block would re-flow the
                 inline content of four other pages to buy them nothing. -->
            <div v-if="fixed" class="cell">
              <slot :name="`cell-${column.key}`" :row="row">
                {{ column.value?.(row) ?? "" }}
              </slot>
            </div>
            <slot v-else :name="`cell-${column.key}`" :row="row">
              {{ column.value?.(row) ?? "" }}
            </slot>
          </td>
        </tr>
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
 * --- Fixed tracks (the `fixed` prop) --------------------------------------
 *
 * The table is exactly as wide as the card and the columns divide that width
 * between them, so no cell can push the card into scrolling sideways.
 */
.table.fixed {
  table-layout: fixed;
}

/* Eleven tracks: the default `--space-4` gutter would spend a third of the
   table's width on padding, and every column would pay for it in text. */
.table.fixed th {
  padding: 0 var(--space-2);
  /* A fixed track is regularly narrower than a two-word header ("Cache read"),
     and `nowrap` in a fixed layout runs the label into its neighbour instead of
     shrinking the column. Two lines of 11px still clear the 36px header. */
  white-space: normal;
  overflow: hidden;
}

.table.fixed td {
  padding: var(--space-2);
}

/*
 * Every cell is exactly two lines tall, so rows are a uniform height and the
 * first line of each column starts on the same baseline.
 *
 * The line box is a length rather than a ratio because the cells are not all
 * text: a badge is ~22px tall (11px type plus its padding and border), and a
 * cell pairing a badge with a line of text has to come out the same height as
 * one holding two lines of text or the rows ripple down the table.
 */
.table.fixed .cell {
  --cell-line: 22px;
  display: -webkit-box;
  -webkit-box-orient: vertical;
  -webkit-line-clamp: 2;
  line-clamp: 2;
  overflow: hidden;
  height: calc(2 * var(--cell-line));
  line-height: var(--cell-line);
  /* A model id or an account slug has no spaces to wrap at, so without this it
     would overrun its one line rather than fill both of them. */
  overflow-wrap: anywhere;
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

  .table td.hide-mobile {
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
    display: block;
    min-width: 0;
    height: auto;
    line-height: inherit;
    overflow: visible;
    /* Both spellings: the `-webkit-` one needs the `-webkit-box` display we
       just dropped, but the standard property clamps a plain block too. */
    -webkit-line-clamp: none;
    line-clamp: none;
  }
}
</style>
