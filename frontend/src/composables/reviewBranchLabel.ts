/**
 * The branch label shared by the review-history list row's chip and the detail
 * drawer's branch field. The list used to carry the target alone while the
 * drawer already rendered `source → target`, so the same review read
 * differently depending on where you looked at it. Both surfaces now call this.
 *
 * The list endpoint does carry the source branch (`branch` on `ReviewListItem`,
 * filled from `SourceMeta::branch` by the backend's `build_review_list_item`),
 * so no field had to be invented — but it is optional in practice: reviews of
 * a local path or a static diff have `SourceMeta::default()` and therefore no
 * branch at all. The fallbacks:
 *
 * - no source → the target alone (never a dangling `→ main`);
 * - source equal to target → one name, not `main → main`;
 * - neither → the empty string, so callers can omit the chip entirely instead
 *   of rendering an empty pill.
 */
export function reviewBranchLabel(
  source: string | null | undefined,
  target: string | null | undefined
): string {
  const from = (source ?? '').trim();
  const to = (target ?? '').trim();
  if (!from || from === to) return to || from;
  return to ? `${from} → ${to}` : from;
}
