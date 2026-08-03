import { request } from './api';
import type { UpgradeCheckResult, UpgradeStartResponse, UpgradeStatus } from '../types/upgrade';

/** `GET /system/upgrade/check` — latest version + install hints (server caches 1h). */
export async function checkUpgrade(): Promise<UpgradeCheckResult> {
  return request('/system/upgrade/check');
}

/**
 * `POST /system/upgrade` — start a binary upgrade (202 `started`), or return
 * docker "notSupported" instructions (200). Brew/cargo/unknown and 409 are
 * surfaced as thrown errors by the request layer (with `.status` attached).
 */
export async function startUpgrade(): Promise<UpgradeStartResponse> {
  return request('/system/upgrade', { method: 'POST' });
}

/** `GET /system/upgrade/status` — job state machine (idle/checking/…/done/failed). */
export async function getUpgradeStatus(): Promise<UpgradeStatus> {
  return request('/system/upgrade/status');
}
