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

export interface UpgradeStatus {
  state: UpgradeJobState
  message: string
  currentVersion: string | null
  targetVersion: string | null
}

/** `POST /system/upgrade` 2xx body: binary starts (202) or docker is unsupported (200). */
export type UpgradeStartResponse =
  | { status: 'started'; targetVersion: string }
  | { status: 'notSupported'; instructions: string; note: string }
