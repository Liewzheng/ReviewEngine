/** One row of a provider card's context menu. */
export interface ContextMenuItem {
  key: string
  label: string
  icon?: unknown
  /** Destructive rows (delete) render in the error colour. */
  destructive?: boolean
  disabled?: boolean
  /** Draw a separator above this row. */
  dividerBefore?: boolean
}
