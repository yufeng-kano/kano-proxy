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
  /** Plain-text value; also the mobile card's value when no slot is given. */
  value?: (row: Row) => string
  /** Hidden on mobile cards — for columns that only add noise there. */
  hideOnMobile?: boolean
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
}>()

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
            :class="{ numeric: column.numeric }"
            :style="column.width ? { width: column.width } : undefined"
          >
            {{ column.header }}
          </th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="row in rows" :key="rowKey(row)">
          <td
            v-for="column in columns"
            :key="column.key"
            :class="{ numeric: column.numeric, 'hide-mobile': column.hideOnMobile }"
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

.numeric {
  text-align: right;
  font-variant-numeric: tabular-nums;
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

  .table td.hide-mobile {
    display: none;
  }
}
</style>
