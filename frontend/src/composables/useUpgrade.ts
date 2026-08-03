import { ref } from 'vue';
import { checkUpgrade, startUpgrade, getUpgradeStatus } from '../services/upgrade';
import type { UpgradeCheckResult, UpgradeStatus } from '../types/upgrade';

// Module-scope singleton: App.vue and UpgradeDialog.vue must share the same
// check/status/polling state, so the state lives at module level (like a
// lightweight store) rather than per-composable-call.

const check = ref<UpgradeCheckResult | null>(null);
const checking = ref(false);
const error = ref<string | null>(null);
const status = ref<UpgradeStatus | null>(null);
const starting = ref(false);
const dialogVisible = ref(false);
const dockerInfo = ref<{ instructions: string; note: string } | null>(null);

let pollTimer: ReturnType<typeof setInterval> | null = null;

const RUNNING_STATES = ['checking', 'downloading', 'verifying', 'installing'];
const TERMINAL_STATES = ['done', 'failed', 'notSupported'];

/** One-shot startup check; server caches the result for 1h, so no polling. */
async function fetchCheck() {
  checking.value = true;
  error.value = null;
  try {
    check.value = await checkUpgrade();
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Unknown error';
  } finally {
    checking.value = false;
  }
}

async function fetchStatus(silent = false) {
  try {
    status.value = await getUpgradeStatus();
  } catch (e) {
    if (!silent) error.value = e instanceof Error ? e.message : 'Unknown error';
  }
}

function stopPolling() {
  if (pollTimer) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
}

/** Poll status every 2s; stop once the job reaches a terminal state. */
function startPolling() {
  stopPolling();
  pollTimer = setInterval(async () => {
    await fetchStatus(true);
    const st = status.value?.state;
    if (st && TERMINAL_STATES.includes(st)) stopPolling();
  }, 2000);
}

/**
 * Start the upgrade. Binary: 202 → immediately reflect `checking` and poll.
 * Docker: 200 notSupported → capture instructions/note. 409 (already in
 * flight): surface the message and resume polling if a job is running.
 */
async function start() {
  if (starting.value) return;
  starting.value = true;
  error.value = null;
  dockerInfo.value = null;
  try {
    const resp = await startUpgrade();
    if (resp.status === 'started') {
      status.value = {
        state: 'checking',
        message: '正在检查最新版本',
        currentVersion: check.value?.currentVersion ?? null,
        targetVersion: resp.targetVersion,
      };
      startPolling();
    } else {
      // docker: 200 notSupported → instructions + note
      dockerInfo.value = { instructions: resp.instructions, note: resp.note };
      await fetchStatus(true);
    }
  } catch (e) {
    const statusCode = (e as { status?: number } | null)?.status;
    if (statusCode === 409) {
      error.value = '升级任务已在进行中，请稍后再试';
      await fetchStatus(true);
      const st = status.value?.state;
      if (st && RUNNING_STATES.includes(st)) startPolling();
    } else {
      error.value = e instanceof Error ? e.message : 'Unknown error';
    }
  } finally {
    starting.value = false;
  }
}

/** Open the dialog and resume any in-flight job / fetch docker instructions. */
async function open() {
  dialogVisible.value = true;
  error.value = null;
  dockerInfo.value = null;
  stopPolling();
  await fetchStatus(true);
  const st = status.value?.state;
  if (st && RUNNING_STATES.includes(st)) startPolling();
  if (check.value?.installMethod === 'docker') {
    await start();
  }
}

function close() {
  dialogVisible.value = false;
  stopPolling();
}

export function useUpgrade() {
  return {
    check,
    checking,
    error,
    status,
    starting,
    dialogVisible,
    dockerInfo,
    fetchCheck,
    fetchStatus,
    start,
    open,
    close,
    stopPolling,
  };
}
