import { computed, onBeforeUnmount, onMounted, ref } from 'vue';

/**
 * Review-history table breakpoints, in `min-width` form (a viewport exactly at
 * the breakpoint still uses the wider set).
 */
export const HISTORY_PROJECT_MIN_WIDTH = 1025;
export const HISTORY_DETAILS_MIN_WIDTH = 769;

export interface HistoryTableColumns {
  /** Project (column 2) — dropped from tablets down. */
  project: boolean;
  /** Author, score, duration and created — dropped from phones down, which
   *  leaves the phone table with title + status + actions. */
  details: boolean;
}

/** Pure breakpoint → column set mapping (the unit tests pin it directly). */
export function historyTableColumns(width: number): HistoryTableColumns {
  return {
    project: width >= HISTORY_PROJECT_MIN_WIDTH,
    details: width >= HISTORY_DETAILS_MIN_WIDTH,
  };
}

/**
 * The review-history table's column set for the current viewport.
 *
 * The narrow layouts drop columns from the `el-table` itself instead of hiding
 * their cells with CSS: a hidden cell still leaves its column in Element Plus's
 * fixed table layout, so the table kept its full width (1026px inside the 302px
 * a phone viewport leaves for it) and the sticky action column slid over the
 * status cell — at 390px the status was only readable by scrolling the table
 * sideways. Removing the column makes Element Plus lay out a table that fits.
 *
 * There is no DOM during SSR or in the node test environment: the width then
 * reads as `Infinity`, so a server render never narrows the table.
 */
export function useHistoryTableColumns() {
  const width = ref(typeof window === 'undefined' ? Number.POSITIVE_INFINITY : window.innerWidth);

  const handleResize = () => {
    width.value = window.innerWidth;
  };

  onMounted(() => window.addEventListener('resize', handleResize));
  onBeforeUnmount(() => window.removeEventListener('resize', handleResize));

  return computed(() => historyTableColumns(width.value));
}
