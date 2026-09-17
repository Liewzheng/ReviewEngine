import { beforeEach, describe, expect, it, vi } from 'vitest';
import { getExperts, updateAggregated } from '../services/experts';
import { useExperts } from './useExperts';

/**
 * RENG-95 — the report-level aggregation toggle.
 *
 * The view's switch is optimistic (mutate → PUT → adopt the server echo →
 * revert on failure), exactly like the per-expert enable switch. The project
 * has no DOM test environment, so the optimistic state machine lives in
 * `useExperts.setAggregated` and is exercised here against Vue's reactivity
 * with the services module mocked; the view's bindings (the switch on the
 * page, the tag on the aggregator card) are pinned against its source in
 * `designLanguage.spec.ts`.
 */

vi.mock('../services/experts', () => ({
  getExperts: vi.fn(),
  updateExpert: vi.fn(),
  updateAggregated: vi.fn(),
}));

const mockedUpdate = vi.mocked(updateAggregated);
const mockedGet = vi.mocked(getExperts);

beforeEach(() => {
  mockedUpdate.mockReset();
  mockedGet.mockReset();
  mockedGet.mockResolvedValue({ experts: [], aggregated: false });
});

describe('useExperts — the aggregation flag', () => {
  it('adopts the effective flag from GET', async () => {
    mockedGet.mockResolvedValue({ experts: [], aggregated: true });
    const store = useExperts();
    expect(store.aggregated.value).toBe(false);
    await store.fetch();
    expect(store.aggregated.value).toBe(true);
  });

  it('calls the endpoint and reflects the server echo on success', async () => {
    mockedUpdate.mockResolvedValue({ aggregated: true, persisted: true });
    const store = useExperts();
    expect(store.aggregated.value).toBe(false);
    // The optimistic flip is visible synchronously at call time.
    const pending = store.setAggregated(true);
    expect(store.aggregated.value, 'the switch flips before the PUT resolves').toBe(true);
    const result = await pending;

    expect(mockedUpdate).toHaveBeenCalledWith(true);
    expect(result.aggregated).toBe(true);
    expect(result.persisted).toBe(true);
    expect(store.aggregated.value).toBe(true);
  });

  it('adopts the server echo even when it differs from the request', async () => {
    // The server is authoritative: it may apply a normalised value.
    mockedUpdate.mockResolvedValue({ aggregated: false });
    const store = useExperts();
    await store.setAggregated(true);
    expect(store.aggregated.value).toBe(false);
  });

  it('reverts to the previous value and rethrows when the PUT fails', async () => {
    mockedUpdate.mockRejectedValue(new Error('HTTP 500'));
    const store = useExperts();
    expect(store.aggregated.value).toBe(false);
    // The optimistic flip is visible immediately (synchronously at call time).
    const pending = store.setAggregated(true);
    expect(store.aggregated.value).toBe(true);
    await expect(pending).rejects.toThrow('HTTP 500');
    expect(store.aggregated.value, 'the optimistic value reverts').toBe(false);
    expect(store.error.value).not.toBeNull();
  });

  it('surfaces a memory-only result without treating it as a failure', async () => {
    // `persisted: false` is not an error: the change applied but is lost on
    // restart. The caller decides how to warn (the view notifies).
    mockedUpdate.mockResolvedValue({ aggregated: true, persisted: false });
    const store = useExperts();
    const result = await store.setAggregated(true);
    expect(result.persisted).toBe(false);
    expect(store.aggregated.value).toBe(true);
  });
});
