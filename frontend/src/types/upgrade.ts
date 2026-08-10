// Upgrade feature types, matching `src/server/api/upgrade.rs`.

export type InstallMethod = 'binary' | 'brew' | 'docker' | 'cargo' | 'unknown'

export interface UpgradeCheckResult {
  currentVersion: string
  latestVersion: string
  updateAvailable: boolean
  installMethod: InstallMethod
  platformAssetAvailable: boolean
  releaseUrl: string
  upgradeHint: string
  cachedAt: string
}

export type UpgradeJobState =
  | 'idle'
  | 'checking'
  | 'downloading'
  | 'verifying'
  | 'installing'
  | 'done'
  | 'failed'
  | 'notSupported'
  // Frontend-only synthetic state: emitted while the container is unreachable
  // during an in-container upgrade restart (the backend is down, so it can
  // never report this state itself).
  | 'restarting'

export interface UpgradeStatus {
  state: UpgradeJobState
  message: string
  currentVersion: string | null
  targetVersion: string | null
}

/** `POST /system/upgrade` 2xx body: binary/docker start the automated job (202);
 *  brew/cargo/unknown may still return `notSupported`. */
export type UpgradeStartResponse =
  | { status: 'started'; targetVersion: string }
  | { status: 'notSupported'; instructions: string; note: string }
