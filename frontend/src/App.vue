<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRoute } from 'vue-router'
import {
  Monitor,
  Document,
  Setting,
  RefreshRight,
  Cpu,
  Tickets,
  User,
  Moon,
  Sunny,
  Key,
  Menu,
} from '@element-plus/icons-vue'
import { ElMessage } from 'element-plus'
import { setApiToken, clearApiToken, getApiToken, onAuthSignal } from './services/api'
import { getAuthStatus, setSystemToken, type AuthStatus } from './services/system'
import BootstrapScreen from './components/Auth/BootstrapScreen.vue'
import UpgradeDialog from './components/Upgrade/UpgradeDialog.vue'
import LanguageSwitcher from './components/common/LanguageSwitcher.vue'
import { useUpgrade } from './composables/useUpgrade'
import { useLocale } from './composables/useLocale'
import { useI18n } from 'vue-i18n'

const { t } = useI18n()
const route = useRoute()
const isDark = ref(true)
const sidebarCollapsed = ref(false)

// --- API token auth state ---
// The authoritative source for "is a token configured" is the backend
// (auth.toml); localStorage only caches the session credential. On first run
// (configured=false) the app shows a full-screen bootstrap screen; once
// configured, a dialog handles unlocking (enter the existing token) and
// rotation (set a new one).
const authPhase = ref<'checking' | 'bootstrap' | 'ready'>('checking')
const bootstrapKeyRequired = ref(false)
const tokenDialogVisible = ref(false)
const tokenDialogMode = ref<'unlock' | 'rotate'>('unlock')
const tokenInput = ref('')
const rotateSaving = ref(false)
const unlockError = ref<string | null>(null)
const unlockDismissed = ref(false)
// True only for the instant an unlock save closes the dialog — lets
// `onTokenDialogClose` distinguish "user dismissed the prompt" from "user
// unlocked successfully" so a later 401 can re-prompt.
const unlockSaved = ref(false)

// Locale infra: Element Plus messages follow the app locale via el-config-provider.
const { elementLocale } = useLocale()

// Upgrade feature: module-scope singleton shared with UpgradeDialog.
const { dialogVisible, check, open, fetchCheck } = useUpgrade()

/**
 * Route auth-related 401 signals to the matching screen. Registered before the
 * first request so a 401 during startup is never misread as a plain error.
 */
function handleAuthSignal(code: string) {
  if (code === 'auth_required') {
    // No token configured server-side (e.g. it was cleared while the app was
    // open): switch to the first-run bootstrap screen.
    authPhase.value = 'bootstrap'
    return
  }
  if (code === 'unauthorized') {
    // A token is configured but this request did not carry a valid one (no
    // token cached, or the cached one is stale/rotated elsewhere). Prompt once
    // per session; the header "API Token" button re-opens the dialog anytime.
    if (authPhase.value === 'bootstrap' || tokenDialogVisible.value || unlockDismissed.value) return
    clearApiToken()
    openUnlockDialog(t('token.invalidToken'))
  }
  // `bootstrap_key_required` is handled inline by the bootstrap screen.
}

function openUnlockDialog(error?: string | null) {
  tokenDialogMode.value = 'unlock'
  tokenInput.value = ''
  unlockError.value = error ?? null
  tokenDialogVisible.value = true
}

function openTokenDialog() {
  // Header button: with a cached token it rotates the server-side token
  // (empty input = keep current); without one it asks for the existing token.
  if (getApiToken()) {
    tokenDialogMode.value = 'rotate'
    tokenInput.value = ''
    unlockError.value = null
    tokenDialogVisible.value = true
  } else {
    openUnlockDialog()
  }
}

function onTokenDialogClose() {
  if (tokenDialogMode.value === 'unlock' && !unlockSaved.value) {
    // User closed the unlock prompt without saving: don't re-nag on every
    // background 401. The header "API Token" button remains available for
    // reopening.
    unlockDismissed.value = true
  }
  unlockSaved.value = false
}

function onCancelTokenDialog() {
  tokenDialogVisible.value = false
}

function clearUnlockError() {
  unlockError.value = null
}

async function saveTokenFromDialog() {
  if (tokenDialogMode.value === 'unlock') {
    const tokenValue = tokenInput.value.trim()
    if (!tokenValue) {
      unlockError.value = t('token.tokenRequiredError')
      return
    }
    // The token already lives on the server (configured=true); this dialog
    // only unlocks this browser session by caching it locally. A wrong token
    // surfaces as 401 unauthorized on the next request, which re-opens the
    // dialog with an "invalid token" hint.
    setApiToken(tokenValue)
    unlockDismissed.value = false
    unlockSaved.value = true
    tokenDialogVisible.value = false
    return
  }

  // Rotate: empty input keeps the current token.
  const newToken = tokenInput.value.trim()
  if (!newToken) {
    tokenDialogVisible.value = false
    return
  }
  rotateSaving.value = true
  unlockError.value = null
  try {
    // PUT /system/token authenticates with the current token (added by
    // request() from localStorage) and persists the new one server-side.
    await setSystemToken(newToken)
    setApiToken(newToken)
    tokenDialogVisible.value = false
    ElMessage.success(t('token.rotateSuccess'))
  } catch (e) {
    const code = (e as { code?: string })?.code
    if (code === 'unauthorized') {
      // The cached token is not accepted by the server (rotated elsewhere or
      // cleared). Switch this dialog to unlock mode so the user enters the
      // existing token instead of a new one.
      clearApiToken()
      tokenDialogMode.value = 'unlock'
      tokenInput.value = ''
      unlockError.value = t('token.invalidToken')
    } else {
      ElMessage.error(t('token.rotateFailed'))
    }
  } finally {
    rotateSaving.value = false
  }
}

/**
 * Resolve the auth phase at startup against the backend's authority. Falls
 * back to the old localStorage heuristic if the backend is unreachable.
 */
async function resolveAuthPhase() {
  let status: AuthStatus | null = null
  try {
    status = await getAuthStatus()
  } catch {
    // Backend unreachable (e.g. dev server without the API up): trust a
    // cached token; otherwise prompt for one.
    status = null
  }
  if (status && !status.configured) {
    bootstrapKeyRequired.value = status.bootstrapKeyRequired
    authPhase.value = 'bootstrap'
    return
  }
  authPhase.value = 'ready'
  if (!getApiToken()) {
    openUnlockDialog()
  }
}

function onBootstrapDone() {
  authPhase.value = 'ready'
  // Re-run the one-shot version/update check now that requests authenticate.
  fetchCheck()
}

onMounted(() => {
  const saved = localStorage.getItem('theme')
  if (saved) {
    isDark.value = saved === 'dark'
  } else {
    isDark.value = true
  }
  document.documentElement.setAttribute('data-theme', isDark.value ? 'dark' : 'light')

  // Register the auth signal handler first, then resolve the phase. The
  // version check waits for phase resolution so its 401 (if any) routes
  // through the right screen instead of racing the bootstrap decision.
  onAuthSignal(handleAuthSignal)
  void resolveAuthPhase().finally(() => {
    fetchCheck()
  })
})

const toggleTheme = () => {
  isDark.value = !isDark.value
  const theme = isDark.value ? 'dark' : 'light'
  document.documentElement.setAttribute('data-theme', theme)
  localStorage.setItem('theme', theme)
}

const toggleSidebar = () => {
  sidebarCollapsed.value = !sidebarCollapsed.value
}

const navItems = [
  { path: '/dashboard', nameKey: 'nav.dashboard', icon: Monitor },
  { path: '/history', nameKey: 'nav.history', icon: Document },
  { path: '/config', nameKey: 'nav.config', icon: Setting },
  { path: '/queue', nameKey: 'nav.queue', icon: RefreshRight },
  { path: '/llm', nameKey: 'nav.llm', icon: Cpu },
  { path: '/logs', nameKey: 'nav.logs', icon: Tickets },
  { path: '/experts', nameKey: 'nav.experts', icon: User },
]

const activeRoute = computed(() => route.path)
const pageTitle = computed(() => {
  const item = navItems.find(i => i.path === route.path)
  return item ? t(item.nameKey) : t('app.name')
})
</script>

<template>
  <el-config-provider :locale="elementLocale">
    <!-- First-run: no token configured server-side → full-screen setup. The
         layout (and its router-view) stays unmounted so no request fires with
         a missing token until one is set. -->
    <BootstrapScreen
      v-if="authPhase === 'bootstrap'"
      :bootstrap-key-required="bootstrapKeyRequired"
      @done="onBootstrapDone"
    />
    <div v-else-if="authPhase === 'ready'" class="app-layout" :class="{ 'sidebar-collapsed': sidebarCollapsed }">
    <!-- Sidebar -->
    <aside class="sidebar">
      <div class="sidebar-brand">
        <span class="brand-icon">🔍</span>
        <span class="brand-text" v-show="!sidebarCollapsed">{{ $t('app.name') }}</span>
      </div>
      <nav class="sidebar-nav">
        <router-link
          v-for="item in navItems"
          :key="item.path"
          :to="item.path"
          class="nav-item"
          :class="{ active: activeRoute === item.path }"
        >
          <el-icon class="nav-icon"><component :is="item.icon" /></el-icon>
          <span class="nav-text" v-show="!sidebarCollapsed">{{ $t(item.nameKey) }}</span>
        </router-link>
      </nav>
      <div class="sidebar-footer">
        <div
          v-if="check?.currentVersion"
          class="version-chip"
          :class="{ 'has-update': check?.updateAvailable }"
          :title="check?.updateAvailable ? $t('header.updateAvailableTitle') : $t('app.name') + ' ' + check?.currentVersion"
        >
          <span class="version-dot"></span>
          <span class="version-text" v-show="!sidebarCollapsed">v{{ check?.currentVersion }}</span>
        </div>
        <button class="theme-toggle" @click="toggleTheme">
          <el-icon><component :is="isDark ? Sunny : Moon" /></el-icon>
        </button>
      </div>
    </aside>

    <!-- Main Area -->
    <div class="main-area">
      <!-- Header -->
      <header class="top-header">
        <button class="menu-toggle" @click="toggleSidebar">
          <el-icon><Menu /></el-icon>
        </button>
        <h1 class="page-title">{{ pageTitle }}</h1>
        <div class="header-actions">
          <el-button text size="small" @click="openTokenDialog">
            <el-icon><Key /></el-icon>
            <span>{{ $t('header.apiToken') }}</span>
          </el-button>
          <LanguageSwitcher />
          <template v-if="check?.updateAvailable">
            <span class="update-tag">{{ $t('header.updateAvailable') }}</span>
            <el-button type="primary" size="small" @click="open">{{ $t('header.upgrade') }}</el-button>
          </template>
          <span class="status-badge healthy">
            <span class="status-dot"></span>
            {{ $t('header.healthy') }}
          </span>
        </div>
      </header>

      <!-- Content -->
      <main class="main-content">
        <router-view />
      </main>
    </div>
  </div>

  <!-- Startup: auth phase not yet resolved — keep the (unauthorized) layout
       from flashing before we know whether bootstrap is needed. -->
  <div v-else class="boot-splash" role="status" aria-label="Loading">
    <span class="brand-icon">🔍</span>
  </div>

  <!-- API Token dialog: unlock (enter the existing token) or rotate (set a new one) -->
  <el-dialog
    v-model="tokenDialogVisible"
    :title="tokenDialogMode === 'rotate' ? $t('token.rotateTitle') : $t('token.title')"
    width="440px"
    :close-on-click-modal="false"
    :close-on-press-escape="false"
    align-center
    @close="onTokenDialogClose"
  >
    <template v-if="tokenDialogMode === 'unlock'">
      <p class="token-hint">{{ $t('token.unlockHint') }}</p>
      <p v-if="unlockError" class="token-error" role="alert">{{ unlockError }}</p>
      <el-input
        v-model="tokenInput"
        type="password"
        :placeholder="$t('token.placeholder')"
        show-password
        clearable
        autocomplete="off"
        @input="clearUnlockError"
        @keyup.enter="saveTokenFromDialog"
      />
    </template>
    <template v-else>
      <p class="token-hint">{{ $t('token.rotateHint') }}</p>
      <el-input
        v-model="tokenInput"
        type="password"
        :placeholder="$t('token.rotatePlaceholder')"
        show-password
        clearable
        autocomplete="new-password"
        @keyup.enter="saveTokenFromDialog"
      />
    </template>
    <template #footer>
      <el-button @click="onCancelTokenDialog">{{ $t('common.cancel') }}</el-button>
      <el-button type="primary" :loading="rotateSaving" @click="saveTokenFromDialog">
        {{ tokenDialogMode === 'rotate' ? $t('token.rotateSave') : $t('common.save') }}
      </el-button>
    </template>
  </el-dialog>

  <!-- Upgrade dialog -->
  <UpgradeDialog v-model="dialogVisible" />
  </el-config-provider>
</template>

<style scoped>
.app-layout {
  display: flex;
  height: 100vh;
  background-color: var(--bg-primary);
}

/* Sidebar */
.sidebar {
  width: var(--sidebar-width);
  background-color: var(--bg-surface);
  border-right: 1px solid var(--border-color);
  display: flex;
  flex-direction: column;
  transition: width 0.3s ease;
  flex-shrink: 0;
}

.sidebar-collapsed .sidebar {
  width: 64px;
}

.sidebar-brand {
  height: var(--header-height);
  display: flex;
  align-items: center;
  padding: 0 16px;
  border-bottom: 1px solid var(--border-color);
  gap: 10px;
}

.brand-icon {
  font-size: 20px;
  flex-shrink: 0;
}

.brand-text {
  font-weight: 600;
  font-size: 16px;
  color: var(--text-primary);
  white-space: nowrap;
  overflow: hidden;
}

.sidebar-nav {
  flex: 1;
  padding: 12px 0;
  overflow-y: auto;
}

.nav-item {
  display: flex;
  align-items: center;
  padding: 10px 16px;
  margin: 2px 8px;
  border-radius: var(--radius-md);
  color: var(--text-secondary);
  text-decoration: none;
  transition: all 0.2s ease;
  gap: 10px;
}

.nav-item:hover {
  background-color: var(--bg-hover);
  color: var(--text-primary);
}

.nav-item.active {
  background-color: var(--bg-active);
  color: var(--brand);
}

.nav-icon {
  font-size: 18px;
  flex-shrink: 0;
}

.nav-text {
  font-size: 14px;
  white-space: nowrap;
  overflow: hidden;
}

.sidebar-footer {
  padding: 12px 16px;
  border-top: 1px solid var(--border-color);
}

.version-chip {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 10px;
  margin-bottom: 8px;
  border-radius: 12px;
  font-family: var(--font-mono);
  font-size: 12px;
  color: var(--text-secondary);
  background: var(--bg-card);
  border: 1px solid var(--border-color);
}

.version-dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--text-secondary);
  flex-shrink: 0;
}

.version-chip.has-update {
  color: var(--success);
  border-color: rgba(34, 197, 94, 0.35);
}

.version-chip.has-update .version-dot {
  background: var(--success);
}

.version-text {
  white-space: nowrap;
  overflow: hidden;
}

.update-tag {
  font-size: 12px;
  font-weight: 500;
  color: var(--success);
  padding: 3px 10px;
  border-radius: 12px;
  background: rgba(34, 197, 94, 0.12);
  border: 1px solid rgba(34, 197, 94, 0.35);
}

.theme-toggle {
  width: 100%;
  padding: 8px;
  border: none;
  border-radius: var(--radius-md);
  background: var(--bg-card);
  color: var(--text-secondary);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
}

.theme-toggle:hover {
  background: var(--bg-hover);
  color: var(--text-primary);
}

/* Main Area */
.main-area {
  flex: 1;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.top-header {
  height: var(--header-height);
  background-color: var(--bg-surface);
  border-bottom: 1px solid var(--border-color);
  display: flex;
  align-items: center;
  padding: 0 20px;
  gap: 16px;
  flex-shrink: 0;
}

.menu-toggle {
  width: 32px;
  height: 32px;
  border: none;
  border-radius: var(--radius-sm);
  background: transparent;
  color: var(--text-secondary);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
}

.menu-toggle:hover {
  background: var(--bg-hover);
  color: var(--text-primary);
}

.page-title {
  font-size: 18px;
  font-weight: 600;
  color: var(--text-primary);
  flex: 1;
}

.header-actions {
  display: flex;
  align-items: center;
  gap: 12px;
}

.status-badge {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 12px;
  border-radius: 12px;
  font-size: 13px;
  font-weight: 500;
  background: rgba(34, 197, 94, 0.15);
  color: var(--success);
}

.status-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--success);
  animation: pulse 2s infinite;
}

@keyframes pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.5; }
}

.main-content {
  flex: 1;
  padding: 20px;
  overflow-y: auto;
}

.token-hint {
  color: var(--text-secondary);
  font-size: 13px;
  line-height: 1.5;
  margin: 0 0 16px 0;
}

.token-error {
  color: var(--error);
  font-size: 13px;
  line-height: 1.5;
  margin: 0 0 12px;
}

.boot-splash {
  height: 100vh;
  display: flex;
  align-items: center;
  justify-content: center;
  background-color: var(--bg-primary);
}

.boot-splash .brand-icon {
  font-size: 40px;
}

/* Mobile */
@media (max-width: 768px) {
  .sidebar {
    position: fixed;
    left: 0;
    top: 0;
    bottom: 0;
    z-index: 100;
    transform: translateX(-100%);
  }

  .sidebar-collapsed .sidebar {
    transform: translateX(0);
    width: var(--sidebar-width);
  }

  .main-area {
    margin-left: 0;
  }
}
</style>
