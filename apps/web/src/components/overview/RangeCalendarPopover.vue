<script setup lang="ts">
/**
 * RangeCalendarPopover: Anchor-attached date picker popover for Day / Week / Month.
 *
 * - Day mode: 30/31-day monthly grid, today marked with a dot, selected day circled,
 *   click any date to view that day's 24-hour hourly curve.
 * - Week mode: 30/31-day monthly grid, this week marked, continuous 7-day strip hover &
 *   selection (Mon–Sun), click any week to view that 7-day daily curve.
 * - Month mode: 12-month annual grid (3×4), this month marked, selected month highlighted,
 *   future months disabled, click any month to view that month's daily curve.
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue"
import ActionIcon from "@/components/ui/ActionIcon.vue"
import AppButton from "@/components/ui/AppButton.vue"
import { useI18n } from "@/i18n"
import type { UsageRangeKind } from "@/types"
import {
  getStartOfDay,
  getStartOfMonth,
  getStartOfWeek,
  isSameDay,
  isSameWeek,
} from "./dateRange"

const props = defineProps<{
  kind: UsageRangeKind
  selectedDate: Date
}>()

const emit = defineEmits<{
  select: [Date]
  close: []
}>()

const { t } = useI18n()

const popoverRef = ref<HTMLElement | null>(null)

// Current viewed month and year in the popover
const viewYear = ref(props.selectedDate.getFullYear())
const viewMonth = ref(props.selectedDate.getMonth()) // 0-11

// Reset view on anchor date / kind changes
watch(
  () => props.selectedDate,
  (d) => {
    viewYear.value = d.getFullYear()
    viewMonth.value = d.getMonth()
  },
)

const today = new Date()

const WEEKDAY_NAMES = computed(() => {
  // Monday-first: Mon, Tue, Wed, Thu, Fri, Sat, Sun
  const base = new Date(2026, 7, 24) // Monday Aug 24, 2026
  return Array.from({ length: 7 }, (_, i) => {
    const d = new Date(base.getFullYear(), base.getMonth(), base.getDate() + i)
    return new Intl.DateTimeFormat("en", { weekday: "short" }).format(d).slice(0, 2)
  })
})

const MONTH_NAMES = computed(() => {
  return Array.from({ length: 12 }, (_, i) => {
    const d = new Date(2026, i, 1)
    return new Intl.DateTimeFormat("en", { month: "short" }).format(d)
  })
})

const viewTitle = computed(() => {
  if (props.kind === "month") {
    return String(viewYear.value)
  }
  const d = new Date(viewYear.value, viewMonth.value, 1)
  return new Intl.DateTimeFormat("en", { month: "long", year: "numeric" }).format(d)
})

const isCurrentViewPeriod = computed(() => {
  if (props.kind === "month") {
    return viewYear.value === today.getFullYear()
  }
  return (
    viewYear.value === today.getFullYear() && viewMonth.value === today.getMonth()
  )
})

const canGoNext = computed(() => {
  if (props.kind === "month") {
    return viewYear.value < today.getFullYear()
  }
  const nextMonthDate = new Date(viewYear.value, viewMonth.value + 1, 1)
  const currentMonthStart = getStartOfMonth(today)
  return nextMonthDate <= currentMonthStart
})

function prev() {
  if (props.kind === "month") {
    viewYear.value--
  } else {
    if (viewMonth.value === 0) {
      viewMonth.value = 11
      viewYear.value--
    } else {
      viewMonth.value--
    }
  }
}

function next() {
  if (!canGoNext.value) return
  if (props.kind === "month") {
    viewYear.value++
  } else {
    if (viewMonth.value === 11) {
      viewMonth.value = 0
      viewYear.value++
    } else {
      viewMonth.value++
    }
  }
}

function jumpCurrent() {
  viewYear.value = today.getFullYear()
  viewMonth.value = today.getMonth()
  if (props.kind === "day") {
    selectDate(today)
  } else if (props.kind === "week") {
    selectDate(getStartOfWeek(today))
  } else {
    selectDate(getStartOfMonth(today))
  }
}

// ---------------------------------------------------------------------------
// Day & Week Grid calculations
// ---------------------------------------------------------------------------
type DayCell = {
  date: Date
  dayNumber: number
  isOtherMonth: boolean
  isToday: boolean
  isSelectedDay: boolean
  isSelectedWeek: boolean
  isFuture: boolean
  isWeekStart: boolean
  isWeekEnd: boolean
}

const hoveredWeekDate = ref<Date | null>(null)

const calendarDays = computed<DayCell[]>(() => {
  const year = viewYear.value
  const month = viewMonth.value

  const firstDay = new Date(year, month, 1)
  const firstDayWeekday = (firstDay.getDay() + 6) % 7 // 0 = Mon, ..., 6 = Sun
  const daysInMonth = new Date(year, month + 1, 0).getDate()
  const daysInPrevMonth = new Date(year, month, 0).getDate()

  const cells: DayCell[] = []
  const todayStart = getStartOfDay(today)

  // Prev month padding
  for (let i = firstDayWeekday - 1; i >= 0; i--) {
    const d = new Date(year, month - 1, daysInPrevMonth - i)
    const dStart = getStartOfDay(d)
    const isFuture = dStart > todayStart
    cells.push({
      date: d,
      dayNumber: d.getDate(),
      isOtherMonth: true,
      isToday: isSameDay(d, today),
      isSelectedDay: isSameDay(d, props.selectedDate),
      isSelectedWeek: isSameWeek(d, props.selectedDate),
      isFuture,
      isWeekStart: (d.getDay() + 6) % 7 === 0,
      isWeekEnd: (d.getDay() + 6) % 7 === 6,
    })
  }

  // Current month
  for (let i = 1; i <= daysInMonth; i++) {
    const d = new Date(year, month, i)
    const dStart = getStartOfDay(d)
    const isFuture = dStart > todayStart
    cells.push({
      date: d,
      dayNumber: i,
      isOtherMonth: false,
      isToday: isSameDay(d, today),
      isSelectedDay: isSameDay(d, props.selectedDate),
      isSelectedWeek: isSameWeek(d, props.selectedDate),
      isFuture,
      isWeekStart: (d.getDay() + 6) % 7 === 0,
      isWeekEnd: (d.getDay() + 6) % 7 === 6,
    })
  }

  // Next month padding to fill complete rows of 7
  const remainder = cells.length % 7
  if (remainder > 0) {
    const fillCount = 7 - remainder
    for (let i = 1; i <= fillCount; i++) {
      const d = new Date(year, month + 1, i)
      const dStart = getStartOfDay(d)
      const isFuture = dStart > todayStart
      cells.push({
        date: d,
        dayNumber: i,
        isOtherMonth: true,
        isToday: isSameDay(d, today),
        isSelectedDay: isSameDay(d, props.selectedDate),
        isSelectedWeek: isSameWeek(d, props.selectedDate),
        isFuture,
        isWeekStart: (d.getDay() + 6) % 7 === 0,
        isWeekEnd: (d.getDay() + 6) % 7 === 6,
      })
    }
  }

  return cells
})

function onDayMouseEnter(cell: DayCell) {
  if (props.kind === "week" && !cell.isFuture) {
    hoveredWeekDate.value = cell.date
  }
}

function onGridMouseLeave() {
  hoveredWeekDate.value = null
}

function isCellHoveredWeek(cell: DayCell): boolean {
  if (props.kind !== "week" || !hoveredWeekDate.value) return false
  return isSameWeek(cell.date, hoveredWeekDate.value)
}

function selectDate(date: Date) {
  emit("select", date)
  emit("close")
}

function onDayClick(cell: DayCell) {
  if (cell.isFuture) return
  if (props.kind === "week") {
    selectDate(getStartOfWeek(cell.date))
  } else {
    selectDate(cell.date)
  }
}

// ---------------------------------------------------------------------------
// Month Grid calculations
// ---------------------------------------------------------------------------
function isMonthThisMonth(monthIndex: number): boolean {
  return viewYear.value === today.getFullYear() && monthIndex === today.getMonth()
}

function isMonthSelected(monthIndex: number): boolean {
  return (
    viewYear.value === props.selectedDate.getFullYear() &&
    monthIndex === props.selectedDate.getMonth()
  )
}

function isMonthFuture(monthIndex: number): boolean {
  if (viewYear.value > today.getFullYear()) return true
  if (viewYear.value === today.getFullYear() && monthIndex > today.getMonth()) return true
  return false
}

function onMonthClick(monthIndex: number) {
  if (isMonthFuture(monthIndex)) return
  selectDate(new Date(viewYear.value, monthIndex, 1))
}

// ---------------------------------------------------------------------------
// Dismiss on click outside & Escape
// ---------------------------------------------------------------------------
function onPointerDown(e: PointerEvent) {
  if (popoverRef.value && !popoverRef.value.contains(e.target as Node)) {
    // Check if clicked the toggle segmented trigger
    const segmentedParent = (e.target as HTMLElement)?.closest(".range-segmented-wrap")
    if (!segmentedParent) {
      emit("close")
    }
  }
}

function onKeydown(e: KeyboardEvent) {
  if (e.key === "Escape") {
    emit("close")
  }
}

onMounted(() => {
  document.addEventListener("pointerdown", onPointerDown, { capture: true })
  document.addEventListener("keydown", onKeydown)
})

onBeforeUnmount(() => {
  document.removeEventListener("pointerdown", onPointerDown, { capture: true })
  document.removeEventListener("keydown", onKeydown)
})
</script>

<template>
  <div
    ref="popoverRef"
    class="calendar-popover"
    role="dialog"
    aria-modal="false"
    :aria-label="t('overview.range.label')"
  >
    <!-- Header -->
    <div class="popover-header">
      <div class="header-nav">
        <AppButton
          icon-only
          size="sm"
          variant="ghost"
          :label="kind === 'month' ? t('overview.calendar.prevYear') : t('overview.calendar.prevMonth')"
          @click="prev"
        >
          <template #icon><ActionIcon name="chevron-left" /></template>
        </AppButton>

        <span class="view-title">{{ viewTitle }}</span>

        <AppButton
          icon-only
          size="sm"
          variant="ghost"
          :disabled="!canGoNext"
          :label="kind === 'month' ? t('overview.calendar.nextYear') : t('overview.calendar.nextMonth')"
          @click="next"
        >
          <template #icon><ActionIcon name="chevron-right" /></template>
        </AppButton>
      </div>

      <button
        v-if="!isCurrentViewPeriod"
        type="button"
        class="jump-today-btn"
        @click="jumpCurrent"
      >
        {{ kind === "day" ? t("overview.calendar.today") : kind === "week" ? t("overview.calendar.thisWeek") : t("overview.calendar.thisMonth") }}
      </button>
    </div>

    <!-- Month & Day calendar grid -->
    <div v-if="kind === 'day' || kind === 'week'" class="calendar-grid-wrap" @mouseleave="onGridMouseLeave">
      <!-- Weekday column headers -->
      <div class="weekday-row">
        <span v-for="(dayName, i) in WEEKDAY_NAMES" :key="i" class="weekday-cell">
          {{ dayName }}
        </span>
      </div>

      <!-- Days matrix -->
      <div class="days-matrix" :class="{ 'week-mode': kind === 'week' }">
        <button
          v-for="(cell, i) in calendarDays"
          :key="i"
          type="button"
          class="day-cell"
          :class="{
            'other-month': cell.isOtherMonth,
            'is-today': cell.isToday,
            'selected-day': kind === 'day' && cell.isSelectedDay,
            'selected-week': kind === 'week' && cell.isSelectedWeek,
            'hovered-week': kind === 'week' && isCellHoveredWeek(cell),
            'week-start': cell.isWeekStart,
            'week-end': cell.isWeekEnd,
            'disabled': cell.isFuture,
          }"
          :disabled="cell.isFuture"
          @click="onDayClick(cell)"
          @mouseenter="onDayMouseEnter(cell)"
        >
          <span class="day-number">{{ cell.dayNumber }}</span>
          <span v-if="cell.isToday && !cell.isSelectedDay" class="today-dot" />
        </button>
      </div>
    </div>

    <!-- Annual 12-month grid -->
    <div v-else class="months-grid">
      <button
        v-for="(monthName, idx) in MONTH_NAMES"
        :key="idx"
        type="button"
        class="month-cell"
        :class="{
          'selected-month': isMonthSelected(idx),
          'this-month': isMonthThisMonth(idx),
          'disabled': isMonthFuture(idx),
        }"
        :disabled="isMonthFuture(idx)"
        @click="onMonthClick(idx)"
      >
        <span class="month-label">{{ monthName }}</span>
        <span v-if="isMonthThisMonth(idx) && !isMonthSelected(idx)" class="today-dot" />
      </button>
    </div>
  </div>
</template>

<style scoped>
.calendar-popover {
  position: absolute;
  top: calc(100% + var(--space-2));
  right: 0;
  z-index: 100;
  width: 276px;
  background: var(--surface);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-lg);
  box-shadow: var(--shadow-lg);
  padding: var(--space-3);
  user-select: none;
  animation: popover-enter var(--duration-fast) var(--ease-enter);
}

@keyframes popover-enter {
  from {
    opacity: 0;
    transform: translateY(-4px) scale(0.98);
  }
  to {
    opacity: 1;
    transform: translateY(0) scale(1);
  }
}

.popover-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: var(--space-2);
}

.header-nav {
  display: flex;
  align-items: center;
  gap: var(--space-1);
}

.view-title {
  font-size: var(--text-sm);
  font-weight: var(--weight-semibold);
  color: var(--text);
  padding: 0 var(--space-1);
  min-width: 110px;
  text-align: center;
}

.jump-today-btn {
  font-size: var(--text-xs);
  font-weight: var(--weight-medium);
  color: var(--muted);
  background: transparent;
  border: none;
  cursor: pointer;
  padding: var(--space-1) var(--space-2);
  border-radius: var(--radius-xs);
  transition: color var(--duration-fast), background var(--duration-fast);
}

.jump-today-btn:hover {
  color: var(--text);
  background: var(--hover);
}

/* Day & Week grid */
.calendar-grid-wrap {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.weekday-row {
  display: grid;
  grid-template-columns: repeat(7, 1fr);
  margin-bottom: 2px;
}

.weekday-cell {
  font-size: var(--text-2xs);
  font-weight: var(--weight-medium);
  color: var(--faint);
  text-align: center;
  padding: 2px 0;
}

.days-matrix {
  display: grid;
  grid-template-columns: repeat(7, 1fr);
  row-gap: 2px;
}

.day-cell {
  position: relative;
  display: inline-flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  height: 32px;
  background: transparent;
  border: none;
  cursor: pointer;
  font-size: var(--text-xs);
  font-weight: var(--weight-normal);
  color: var(--text);
  padding: 0;
  transition: background var(--duration-fast), color var(--duration-fast);
}

.day-cell.other-month {
  color: var(--faint);
  opacity: 0.55;
}

.day-cell.disabled {
  opacity: 0.25;
  cursor: not-allowed;
}

/* Day mode hover and select */
:not(.week-mode) .day-cell:not(.disabled):not(.selected-day):hover {
  background: var(--hover);
  border-radius: var(--radius-full);
}

.day-cell.selected-day {
  background: var(--accent);
  color: var(--accent-fg);
  font-weight: var(--weight-semibold);
  border-radius: var(--radius-full);
}

/* Week mode continuous strip */
.week-mode .day-cell.week-start {
  border-top-left-radius: var(--radius-sm);
  border-bottom-left-radius: var(--radius-sm);
}

.week-mode .day-cell.week-end {
  border-top-right-radius: var(--radius-sm);
  border-bottom-right-radius: var(--radius-sm);
}

.week-mode .day-cell.hovered-week:not(.selected-week):not(.disabled) {
  background: var(--hover);
}

.week-mode .day-cell.selected-week {
  background: var(--accent);
  color: var(--accent-fg);
  font-weight: var(--weight-semibold);
}

/* Indicators */
.today-dot {
  position: absolute;
  bottom: 3px;
  width: 4px;
  height: 4px;
  border-radius: var(--radius-full);
  background: var(--chart-input);
}

/* Month Grid (3x4) */
.months-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: var(--space-2);
  padding: var(--space-1) 0;
}

.month-cell {
  position: relative;
  display: flex;
  align-items: center;
  justify-content: center;
  height: 40px;
  background: var(--surface-2);
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  cursor: pointer;
  font-size: var(--text-sm);
  font-weight: var(--weight-medium);
  color: var(--text);
  transition: background var(--duration-fast), border-color var(--duration-fast), color var(--duration-fast);
}

.month-cell:not(.disabled):not(.selected-month):hover {
  background: var(--hover);
  border-color: var(--border-strong);
}

.month-cell.selected-month {
  background: var(--accent);
  border-color: var(--accent);
  color: var(--accent-fg);
  font-weight: var(--weight-semibold);
}

.month-cell.disabled {
  opacity: 0.25;
  cursor: not-allowed;
}

.month-cell .today-dot {
  bottom: 4px;
}
</style>
