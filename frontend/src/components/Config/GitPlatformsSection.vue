<template>
  <!-- Git Platforms Card -->
  <el-card class="config-card git-platforms-card">
    <template #header>
      <div class="card-header">
        <el-icon><Connection /></el-icon>
        <span>{{ $t('config.gitPlatforms.title') }}</span>
        <div class="header-action">
          <el-button size="small" type="primary" @click="openAddDialog">
            <el-icon><Plus /></el-icon>
            {{ $t('config.gitPlatforms.addBtn') }}
          </el-button>
        </div>
      </div>
    </template>
    <div class="card-body">
      <el-empty
        v-if="platforms.length === 0"
        :description="$t('config.gitPlatforms.empty')"
        :image-size="80"
      >
        <el-button size="small" type="primary" @click="openAddDialog">
          <el-icon><Plus /></el-icon>
          {{ $t('config.gitPlatforms.addBtn') }}
        </el-button>
      </el-empty>
      <div v-else class="platforms-list">
        <div v-for="row in rows" :key="row.platform.name" class="platform-item">
          <div class="platform-item-header">
            <div class="platform-item-info">
              <el-tag size="small">{{ row.platform.type }}</el-tag>
              <span class="platform-item-name">{{ row.platform.name }}</span>
              <span class="platform-item-base">{{ row.platform.baseUrl }}</span>
              <span v-if="row.platform.token" class="platform-item-token is-set">••••••••</span>
              <span v-else class="platform-item-token">{{ $t('config.notSet') }}</span>
            </div>
            <div class="platform-item-actions">
              <!-- The connectivity probe is a read-only check that posts the
                   row's (possibly masked) token to the server-side probe. -->
              <el-button
                size="small"
                text
                :loading="testingIndex === row.index"
                @click="testPlatform(row.index)"
              >
                {{ $t('config.gitPlatforms.test') }}
              </el-button>
              <el-button size="small" text @click="openEditDialog(row.index)">
                {{ $t('common.edit') }}
              </el-button>
              <el-button size="small" text type="danger" @click="confirmRemove(row.index)">
                <el-icon><Delete /></el-icon>
              </el-button>
            </div>
          </div>
          <!-- Last probe result (RENG-54): session state, so the page's 10s
               poll — which replaces the whole `gitPlatforms` array — cannot
               wipe the outcome the user just asked for. -->
          <TestResultLine
            v-if="row.test"
            class="platform-item-test"
            :type="row.test.type"
            :text="row.test.text"
            :at="row.test.at"
            @dismiss="testResults.clear(row.platform.name)"
          />
        </div>
      </div>
    </div>
  </el-card>

  <!-- Add / Edit Git Platform Dialog (RENG-96: secret fields show the `***`
       mask for a configured value, with an explicit clear button). -->
  <GitPlatformDialog
    :open="showDialog"
    :mode="dialogMode"
    :platform="dialogMode === 'edit' ? props.platforms[editingIndex] : undefined"
    :platforms="props.platforms"
    :editing-index="editingIndex"
    @save="onDialogSave"
    @close="showDialog = false"
  />
</template>

<script setup lang="ts">
import { computed, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import { Connection, Delete, Plus } from '@element-plus/icons-vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import type { GitPlatformConfig } from '../../types/config';
import { testGitPlatform, type GitPlatformTestResult } from '../../services/config';
import { useTransientResult } from '../../composables/useTransientResult';
import TestResultLine from '../common/TestResultLine.vue';
import GitPlatformDialog from './GitPlatformDialog.vue';

const props = defineProps<{
  /** Configured git platform entries (secrets masked as returned by GET /config). */
  platforms: GitPlatformConfig[];
}>();

const emit = defineEmits<{
  /** Stage a new platform entry; the page's auto-save persists it. */
  add: [entry: GitPlatformConfig];
  /** Replace the entry at `index`; the page's auto-save persists it. */
  edit: [index: number, entry: GitPlatformConfig];
  /** Drop the entry at `index`; the page's auto-save persists it. */
  remove: [index: number];
}>();

const { t } = useI18n();

// --- Add / Edit dialog state (the form itself lives in GitPlatformDialog) ---
const showDialog = ref(false);
const dialogMode = ref<'add' | 'edit'>('add');
/** Index of the row being edited; -1 when adding. */
const editingIndex = ref(-1);

// --- Test state ---
/** Row whose connectivity probe is in flight (null when idle). */
const testingIndex = ref<number | null>(null);
/**
 * Last probe result per platform NAME (RENG-54). Session state held OUTSIDE
 * the config model: the page polls `GET /config` every 10s and `applyConfig`
 * replaces `gitPlatforms` wholesale, so a result stored on the row would be
 * gone on the next tick.
 *
 * The key is the platform's name — the row's identity in the config (`name`
 * is the match key the backend itself uses for secrets) and the list's
 * `v-for` key — so it is stable across polls, edits and removals. It is NOT
 * an array index, which shifts as soon as a row above it is removed.
 * A rename through the edit dialog changes the identity, so an edit clears
 * the result under both the old and the new name rather than letting the
 * renamed platform inherit the previous platform's probe.
 *
 * Written only by `testPlatform`; cleared by the row's dismiss control, by an
 * edit, by removing the platform, or by leaving the page.
 */
const testResults = useTransientResult<GitPlatformTestResult>();

/**
 * Rows for the list: the platform plus the outcome of its last probe, so the
 * template binds one object per row instead of re-deriving the result.
 */
const rows = computed(() =>
  props.platforms.map((platform, index) => ({
    platform,
    index,
    test: platformTestLine(platform.name),
  }))
);

/** Presentation for a row's recorded probe (null when it was never tested). */
function platformTestLine(name: string) {
  const recorded = testResults.get(name);
  if (!recorded) return null;
  return {
    type: recorded.value.ok ? ('success' as const) : ('danger' as const),
    text: recorded.value.ok
      ? t('config.gitPlatforms.testOk', { version: recorded.value.version ?? '?' })
      : t('config.gitPlatforms.testFailed', {
          error: recorded.value.error ?? t('errors.unknown'),
        }),
    at: recorded.at,
  };
}

function openAddDialog() {
  dialogMode.value = 'add';
  editingIndex.value = -1;
  showDialog.value = true;
}

function openEditDialog(index: number) {
  dialogMode.value = 'edit';
  editingIndex.value = index;
  showDialog.value = true;
}

/** The dialog emitted a validated entry: stage it on the page's config (the
 * debounced auto-save PUTs it). RENG-96: the entry carries the row's `id`
 * (echoed from GET), so the backend updates that entry — and keeps its
 * credentials — however name/baseUrl changed. */
function onDialogSave(entry: GitPlatformConfig) {
  if (dialogMode.value === 'edit') {
    const original = props.platforms[editingIndex.value];
    emit('edit', editingIndex.value, entry);
    // The configuration that was probed just changed, so the recorded
    // result no longer describes this platform (RENG-54). The dialog can
    // also RENAME the row — the result key is the name — so drop both: the
    // old identity must not linger, and the renamed row must not inherit a
    // probe it never ran.
    testResults.clear(original.name);
    testResults.clear(entry.name);
  } else {
    emit('add', entry);
  }
  showDialog.value = false;
}

/**
 * Probe a platform's connectivity. The row's token is sent as-is: a masked
 * (`***`) or blank token falls back server-side to the stored token of the
 * platform — matched by id first (RENG-96), else by baseUrl. The endpoint
 * always answers HTTP 200, so probe failures arrive in the body; only
 * network/HTTP errors hit catch.
 *
 * The outcome — either path — is recorded against the platform name as
 * page-session state, next to the toast, so the row keeps showing the result
 * of the probe instead of losing it on the next 10s poll (RENG-54).
 */
async function testPlatform(index: number) {
  const platform = props.platforms[index];
  testingIndex.value = index;
  try {
    const result = await testGitPlatform({
      baseUrl: platform.baseUrl,
      token: platform.token,
      id: platform.id,
    });
    testResults.set(platform.name, result);
    if (result.ok) {
      ElMessage.success(t('config.gitPlatforms.testOk', { version: result.version ?? '?' }));
    } else {
      ElMessage.error(
        t('config.gitPlatforms.testFailed', { error: result.error ?? t('errors.unknown') })
      );
    }
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    testResults.set(platform.name, { ok: false, error: message });
    ElMessage.error(t('config.gitPlatforms.testFailed', { error: message }));
  } finally {
    testingIndex.value = null;
  }
}

/** Ask for confirmation, then stage the row for deletion on save. */
function confirmRemove(index: number) {
  const name = props.platforms[index].name;
  ElMessageBox.confirm(
    t('config.gitPlatforms.removeConfirm', { name }),
    t('config.gitPlatforms.removeTitle'),
    {
      confirmButtonText: t('common.remove'),
      cancelButtonText: t('common.cancel'),
      type: 'warning',
    }
  )
    .then(() => {
      testResults.clear(name);
      emit('remove', index);
    })
    .catch(() => {
      /* cancelled */
    });
}
</script>

<style scoped>
.card-header {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  font-weight: 500;
  font-size: 14px;
  color: var(--text-primary);
}

.card-body {
  padding: 20px;
}

.header-action {
  margin-left: auto;
}

.git-platforms-card :deep(.el-card__body) {
  padding: var(--space-4) 20px;
}

.platforms-list {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
}

.platform-item {
  border: 1px solid var(--border-color);
  border-radius: var(--radius-md);
  background: var(--bg-surface);
  overflow: hidden;
  transition: border-color 0.2s ease, box-shadow 0.2s ease;
}

.platform-item:hover {
  border-color: var(--brand);
  box-shadow: 0 0 0 1px var(--brand);
}

.platform-item-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: var(--space-3) var(--space-4);
  gap: var(--space-3);
}

.platform-item-info {
  display: flex;
  align-items: center;
  gap: 10px;
  flex: 1;
  min-width: 0;
  overflow: hidden;
}

/* RENG-54: the last probe's outcome, one line under the row it belongs to. */
.platform-item-test {
  padding: 0 var(--space-4) var(--space-3);
}

.platform-item-name {
  font-size: 13px;
  font-weight: 500;
  color: var(--text-primary);
  white-space: nowrap;
}

.platform-item-base {
  font-size: 12px;
  color: var(--text-secondary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.platform-item-token {
  font-size: 12px;
  color: var(--text-secondary);
  white-space: nowrap;
  flex-shrink: 0;
}

.platform-item-token.is-set {
  font-family: var(--font-mono);
  letter-spacing: 2px;
}

.platform-item-actions {
  display: flex;
  align-items: center;
  gap: var(--space-1);
  flex-shrink: 0;
}

/* Info icon next to form labels; hover/focus reveals the tooltip */
.help-icon {
  margin-left: var(--space-1);
  font-size: 14px;
  vertical-align: text-bottom;
  color: var(--text-secondary);
  cursor: help;
}

.help-icon:focus-visible {
  outline: 2px solid var(--accent-primary);
  outline-offset: 1px;
  border-radius: 50%;
}

@media (max-width: 767px) {
  .card-body {
    padding: var(--space-4);
  }
}
</style>
