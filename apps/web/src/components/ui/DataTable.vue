<!--
  The one place table markup lives.

  Three things a page's own `<table>` reliably gets wrong, all handled here:

  1. Sticky header — the column names stay while the body scrolls inside its
     card, which is what lets a long table live in a bounded region instead of
     growing the page (docs/admin-ui.md § Anti-scroll rules).
  2. Alignment — numeric columns are right-aligned with tabular numerals,
     declared once per column rather than re-specified per cell.
  3. Mobile — below 768px each row becomes a card of label/value pairs.
     Horizontal scroll on a phone hides exactly the columns that matter, so
     that is not the fallback.

  Cells render through a per-column slot named `cell-<key>`, falling back to
  the column's `value()`.
-->

<!-- A plain block, not a second `<script setup>`: only one setup block is
     compiled, and a type exported from a second one is silently dropped along
     with every macro in it. -->
<script lang="ts">
export type Column<Row> = {
  key: string
  header: string
  /** Right-aligned + tabular numerals. */
  numeric?: boolean
  /**
   * Right-aligns the header *and* the cells without the numeric treatment —
   * for a control column (Copy, Revoke). Alignment belongs to the column, not
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
   */
  width?: string
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
}>()

defineEmits<{ rowClick: [Row] }>()

defineSlots<Record<string, (props: { row: Row }) => unknown>>()
</script>

<template>
  <div class="table-wrap">
    <table class="table">
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
            {{ column.header }}
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
            :data-label="column.header"
          >
            <slot :name="`cell-${column.key}`" :row="row">
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

.numeric {
  text-align: right;
  font-variant-numeric: tabular-nums;
}

/* Right-aligned without the numeric treatment: a control column, where
   tabular figures would do nothing and the header has to sit over the
   control rather than at the far edge of the track. */
.end {
  text-align: right;
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
}
</style>
