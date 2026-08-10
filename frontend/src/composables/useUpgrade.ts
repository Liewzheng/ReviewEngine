import { ref } from 'vue';
import { checkUpgrade, startUpgrade, getUpgradeStatus } from '../services/upgrade';
import { i18n } from '../i18n';
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

let pollTimer: ReturnType<typeof setInterval> | null = null;
let restartProbeInterval: ReturnType<typeof setInterval> | null = null;
let restartProbeBackstop: ReturnType<typeof setTimeout> | null = null;

const RUNNING_STATES = ['checking', 'downloading', 'verifying', 'installing'];
const TERMINAL_STATES = ['done', 'failed', 'notSupported'];

// Poll cadence while an upgrade job is running.
const POLL_INTERVAL = 2000;
// Once "container restarting" is detected, probe every 2s for the server to
// come back; if it stays unreachable past this window, hard-reload anyway.
const RESTART_PROBE_INTERVAL = 2000;
const RESTART_RELOAD_DELAY = 12000;

function isDockerUpgrade(): boolean {
  return check.value?.installMethod === 'docker';
}

/** True while the upgrade could still be running inside this container process. */
function isUpgradeInFlight(): boolean {
  const st = status.value?.state;
  if (!st) return false;
  if (RUNNING_STATES.includes(st)) return true;
  if (st === 'restarting') return true;
  // For docker, "done" is followed by an automatic container restart; a
  // connection loss right after done still means "restarting", never an error.
  return isDockerUpgrade() && st === 'done';
}

function stopPolling() {
  if (pollTimer) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
}

function clearRestartProbe() {
  if (restartProbeInterval) {
    clearInterval(restartProbeInterval);
    restartProbeInterval = null;
  }
  if (restartProbeBackstop) {
    clearTimeout(restartProbeBackstop);
    restartProbeBackstop = null;
  }
}

/** One-shot startup check; server caches the result for 1h, so no polling. */
async function fetchCheck() {
  checking.value = true;
  error.value = null;
  try {
    check.value = await checkUpgrade();
  } catch (e) {
    error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
  } finally {
    checking.value = false;
  }
}

/**
 * Fetch the job state. On failure during an in-flight upgrade, the process has
 * likely exited to replace its own binary (container restart) — enter the
 * `restarting` state instead of surfacing an error. Otherwise surface the
 * error only for non-silent callers (e.g. the dialog open probe); the poll
 * loop itself is silent.
 */
async function fetchStatus(silent = false): Promise<UpgradeStatus | null> {
  try {
    const st = await getUpgradeStatus();
    status.value = st;
    return st;
  } catch (e) {
    if (isUpgradeInFlight()) {
      enterRestarting();
      return null;
    }
    if (!silent) error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
    return null;
  }
}

/** Poll loop while a job is running. */
async function pollTick() {
  const prev = status.value?.state;
  const st = await fetchStatus(true);
  if (!st) return;
  // A fresh idle process right after a docker upgrade means the container
  // restarted — load the new build.
  if (
    isDockerUpgrade() &&
    st.state === 'idle' &&
    (prev === 'done' || prev === 'restarting' || RUNNING_STATES.includes(prev ?? ''))
  ) {
    location.reload();
    return;
  }
  // For docker "done" we keep polling so the automatic container restart is
  // caught (it either drops the connection or comes back as a fresh idle
  // process). Binary stops at the first terminal state.
  const isDockerDone = isDockerUpgrade() && st.state === 'done';
  if (!isDockerDone && TERMINAL_STATES.includes(st.state)) {
    stopPolling();
  }
}

/** Poll status every 2s while a job is running. */
function startPolling() {
  stopPolling();
  pollTimer = setInterval(pollTick, POLL_INTERVAL);
}

/**
 * Enter the "container restarting" UI state. The process is replacing its own
 * binary and the container is coming back up, so the status endpoint is
 * unreachable. Probe for the server to return and reload the page with the new
 * build; if it stays down past the window, hard-reload anyway.
 */
function enterRestarting() {
  if (status.value?.state === 'restarting') return;
  status.value = {
    state: 'restarting',
    message: i18n.global.t('upgrade.restartHint'),
    currentVersion: check.value?.currentVersion ?? null,
    targetVersion: check.value?.latestVersion ?? null,
  };
  stopPolling();
  clearRestartProbe();
  restartProbeBackstop = setTimeout(() => location.reload(), RESTART_RELOAD_DELAY);
  restartProbeInterval = setInterval(async () => {
    try {
      const st = await getUpgradeStatus();
      if (st.state === 'idle') {
        // fresh process → container restarted → load the new build
        location.reload();
        return;
      }
      // Server is reachable again but the job is still running: the "restart"
      // was a transient network blip. Exit restarting and resume polling.
      clearRestartProbe();
      status.value = st;
      startPolling();
    } catch {
      // still down — keep waiting for the backstop window
    }
  }, RESTART_PROBE_INTERVAL);
}

/**
 * Start the upgrade. Binary and docker both trigger the automated in-process
 * flow (202 → start polling). A defensive `notSupported` response (older
 * backend) is surfaced as an error rather than host commands. 409 (already in
 * flight) surfaces the message and resumes polling if a job is running.
 */
async function start() {
  if (starting.value) return;
  starting.value = true;
  error.value = null;
  clearRestartProbe();
  try {
    const resp = await startUpgrade();
    if (resp.status === 'started') {
      status.value = {
        state: 'checking',
        message: i18n.global.t('upgrade.checkingLatest'),
        currentVersion: check.value?.currentVersion ?? null,
        targetVersion: resp.targetVersion,
      };
      startPolling();
    } else {
      // Defensive: pre-docker-automation backend returns notSupported.
      error.value = i18n.global.t('upgrade.notSupportedError');
    }
  } catch (e) {
    const statusCode = (e as { status?: number } | null)?.status;
    if (statusCode === 409) {
      error.value = i18n.global.t('upgrade.inProgressError');
      await fetchStatus(true);
      const st = status.value?.state;
      if (st && RUNNING_STATES.includes(st)) startPolling();
    } else {
      error.value = e instanceof Error ? e.message : i18n.global.t('errors.unknown');
    }
  } finally {
    starting.value = false;
  }
}

/** Open the dialog and resume any in-flight job (the user confirms the start). */
async function open() {
  dialogVisible.value = true;
  error.value = null;
  stopPolling();
  await fetchStatus(true);
  const st = status.value?.state;
  if (st && RUNNING_STATES.includes(st)) startPolling();
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
    fetchCheck,
    fetchStatus,
    start,
    open,
    close,
    stopPolling,
  };
}
