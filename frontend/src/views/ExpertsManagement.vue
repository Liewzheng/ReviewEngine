<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount, watch } from 'vue'
import { useRouter } from 'vue-router'
import {
  Plus,
  Close,
  Search,
  WarningFilled,
} from '@element-plus/icons-vue'
import { ElNotification } from 'element-plus'
import { useI18n } from 'vue-i18n'
import type { Expert, ExpertCategory, ExpertReviewSummary } from '../types/expert'
import { categoryLabelMap } from '../types/expert'
import { useExperts } from '../composables/useExperts'
import { useAutoRefresh } from '../composables/useAutoRefresh'
import { usePromptEditor, MAX_EXPERT_PROMPT_CHARS } from '../composables/usePromptEditor'
import ExpertCard from '../components/ExpertsManagement/ExpertCard.vue'
import PageHeader from '../components/common/PageHeader.vue'

const router = useRouter()
const { t } = useI18n()

// ========== Composable ==========
const expertsStore = useExperts()

// ========== State ==========
const experts = expertsStore.experts
const loading = expertsStore.loading
const detailModalVisible = ref(false)
const selectedExpert = ref<Expert | null>(null)
const searchQuery = ref('')
const filterCategory = ref<ExpertCategory | 'all'>('all')

/* ─────────────── Error Handling ─────────────── */
watch(() => expertsStore.error.value, (err) => {
  if (err) {
    ElNotification({
      title: t('common.error'),
      message: err,
      type: 'error',
      duration: 5000,
    })
  }
})

// ========== Computed ==========
const categories = computed(() => [
  { value: 'all', label: t('experts.categories.all') },
  ...Object.entries(categoryLabelMap).map(([value, label]) => ({ value, label })),
])

const filteredExperts = computed(() => {
  let result = experts.value
  if (filterCategory.value !== 'all') {
    result = result.filter((e: Expert) => e.category === filterCategory.value)
  }
  if (searchQuery.value.trim()) {
    const q = searchQuery.value.toLowerCase()
    result = result.filter((e: Expert) =>
      e.name.toLowerCase().includes(q) ||
      e.description.toLowerCase().includes(q) ||
      categoryLabelMap[e.category].toLowerCase().includes(q)
    )
  }
  return result
})

const enabledCount = computed(() => experts.value.filter((e: Expert) => e.enabled).length)
const totalCount = computed(() => experts.value.length)
const avgWeight = computed(() => {
  const enabled = experts.value.filter((e: Expert) => e.enabled)
  if (enabled.length === 0) return 0
  return Math.round(enabled.reduce((sum: number, e: Expert) => sum + e.weight, 0) / enabled.length)
})

// ========== Methods ==========
const fetchExperts = async (silent: boolean = false) => {
  await expertsStore.fetch(silent)
}

/* Every card control is live: the switch and slider optimistically mutate the
 * local expert, PUT to the server, and roll back + notify on failure. */

/* Count of expert PUTs in flight. A background tick is skipped while one is
 * in flight (see `expertsAutoRefresh`): the poll reconciles the server's list
 * onto the local objects in place, so a tick landing between the optimistic
 * write and the server's answer would revert the switch (or the slider) to
 * the value the server had before the PUT — the value the user just changed
 * would visibly snap back for a whole poll interval (RENG-54). */
const savesInFlight = ref(0)

const handleToggle = async (id: string, enabled: boolean) => {
  const expert = experts.value.find((e: Expert) => e.id === id)
  if (!expert) return
  const previous = expert.enabled
  expert.enabled = enabled
  savesInFlight.value++
  try {
    const updated = await expertsStore.update(id, { enabled })
    if (updated.persisted === false) {
      // RENG-69: the server applied the change but has no store attached, so
      // it dies with the container. Never report that as a clean success.
      notifyMemoryOnly(expert.name)
    } else {
      ElNotification({
        title: enabled ? t('experts.toggle.enabledTitle') : t('experts.toggle.disabledTitle'),
        message: enabled ? t('experts.toggle.enabledMessage', { name: expert.name }) : t('experts.toggle.disabledMessage', { name: expert.name }),
        type: enabled ? 'success' : 'warning',
        duration: 2000,
      })
    }
  } catch (e) {
    expert.enabled = previous
    notifyUpdateFailed(expert.name, e)
  } finally {
    savesInFlight.value--
  }
}

/* Weight slider drags emit per pixel: debounce the PUT (500ms after the last
 * movement) and remember the pre-drag value for rollback. The whole debounce
 * window is gated via `weightDragPending` so a background tick can neither
 * replace the list nor snap the slider back mid-drag; `savesInFlight`
 * additionally covers the commit await itself. */
const WEIGHT_DEBOUNCE_MS = 500
const weightTimers = new Map<string, ReturnType<typeof setTimeout>>()
const previousWeights = new Map<string, number>()
/** True from the first drag movement until the debounced commit fires. */
const weightDragPending = ref(false)

const handleWeightChange = (id: string, weight: number) => {
  const expert = experts.value.find((e: Expert) => e.id === id)
  if (!expert) return
  if (!previousWeights.has(id)) previousWeights.set(id, expert.weight)
  expert.weight = weight
  weightDragPending.value = true
  const existing = weightTimers.get(id)
  if (existing) clearTimeout(existing)
  weightTimers.set(id, setTimeout(() => commitWeight(id), WEIGHT_DEBOUNCE_MS))
}

const commitWeight = async (id: string) => {
  weightTimers.delete(id)
  if (weightTimers.size === 0) weightDragPending.value = false
  const expert = experts.value.find((e: Expert) => e.id === id)
  const previous = previousWeights.get(id)
  previousWeights.delete(id)
  if (!expert || previous === undefined || expert.weight === previous) return
  savesInFlight.value++
  try {
    const updated = await expertsStore.update(id, { weight: expert.weight })
    if (updated.persisted === false) {
      // Same honesty rule as the toggle: the slider committed, but without a
      // store the value will not survive a restart.
      notifyMemoryOnly(expert.name)
    }
  } catch (e) {
    expert.weight = previous
    notifyUpdateFailed(expert.name, e)
  } finally {
    savesInFlight.value--
  }
}

function notifyUpdateFailed(name: string, error: unknown) {
  console.error('Failed to update expert', error)
  ElNotification({
    title: t('common.error'),
    message: t('experts.updateFailed', { name }),
    type: 'error',
    duration: 5000,
  })
}

/* RENG-93: the prompt editor. The textarea edits a local draft (see
 * `usePromptEditor`); saving is explicit, never per keystroke. A save is an
 * expert PUT like any other, so it is gated by the same `savesInFlight` poll
 * pause, and the draft itself pauses polling while unsaved (like
 * `weightDragPending`) so a background refresh can neither snap the textarea
 * back nor relabel the source hint under the user. */
const promptEditor = usePromptEditor(selectedExpert, async (id, prompt) => {
  const updated = await expertsStore.update(id, { prompt })
  if (updated.persisted === false) {
    // Same honesty rule as the toggle: the prompt saved, but without a store
    // it will not survive a restart.
    notifyMemoryOnly(updated.name)
  }
  return updated
})
const promptDraft = promptEditor.draft
const promptDirty = promptEditor.dirty
const promptSaving = promptEditor.saving
const promptSaveState = promptEditor.saveState
const setPromptDraft = promptEditor.setDraft
const savePrompt = promptEditor.savePrompt

function notifyPromptSaveFailed(error: unknown) {
  console.error('Failed to save expert prompt', error)
  ElNotification({
    title: t('common.error'),
    message: t('experts.detail.promptSaveFailed'),
    type: 'error',
    duration: 5000,
  })
}

/* The button's handler: `savePrompt` already reverts the draft on failure
 * (the server value stands), so all that is left is to say so. */
const onSavePrompt = async () => {
  try {
    await savePrompt()
  } catch (e) {
    notifyPromptSaveFailed(e)
  }
}

/* RENG-69: the server applied the edit but could not (or would not) store it,
 * so it is lost on restart. A warning, not a success — the user must not
 * believe the configuration is durable. */
function notifyMemoryOnly(name: string) {
  ElNotification({
    title: t('experts.memoryOnlyTitle'),
    message: t('experts.memoryOnlyMessage', { name }),
    type: 'warning',
    duration: 5000,
  })
}

const handleViewDetails = (expert: Expert) => {
  selectedExpert.value = expert
  detailModalVisible.value = true
}

const handleRowClick = (row: ExpertReviewSummary) => {
  router.push(`/history?reviewId=${row.reviewId}`)
}

const getScoreType = (score?: number): 'success' | 'warning' | 'danger' | 'info' => {
  if (!score) return 'info'
  if (score >= 90) return 'success'
  if (score >= 70) return 'warning'
  return 'danger'
}

// ========== Lifecycle ==========
/* Background auto-refresh (10s), e.g. to reflect experts changed elsewhere.
 * The tick is silent: it never flips `loading` (so the grid — and any open
 * detail dialog — is never unmounted by a poll) and reconciles the fetched
 * list in place, preserving expert object identities. Ticks are additionally
 * skipped while a local edit is in flight — a weight-slider debounce window
 * (`weightDragPending`) or any expert PUT (`savesInFlight`, RENG-54) — so a
 * poll can neither replace the list, snap a slider back mid-drag, nor revert
 * an optimistic switch to the pre-PUT server value. The initial load and any
 * later remount path do a normal, visible fetch. */
const expertsAutoRefresh = useAutoRefresh(
  () => fetchExperts(true),
  10_000,
  {
    // RENG-54 + RENG-93: skip a tick while a local edit is up — a weight
    // drag window, any expert PUT, or an unsaved prompt draft — so a poll can
    // neither replace the list, snap a slider back mid-drag, revert an
    // optimistic switch, nor overwrite a prompt the user is still editing.
    isPaused: () =>
      weightDragPending.value || savesInFlight.value > 0 || promptDirty.value,
  }
)

onMounted(() => {
  fetchExperts()
  expertsAutoRefresh.start()
})

onBeforeUnmount(() => {
  expertsAutoRefresh.stop()
  weightTimers.forEach((timer) => clearTimeout(timer))
  weightTimers.clear()
})
</script>

<template>
  <div class="experts-page">
    <PageHeader :title="$t('experts.title')" :subtitle="$t('experts.subtitle')">
      <template #actions>
        <el-tooltip :content="$t('experts.comingSoon')" placement="top">
          <el-button type="primary" disabled :aria-label="$t('experts.addExpertComingSoonAria')">
            <el-icon><Plus /></el-icon>
            {{ $t('experts.addExpert') }}
          </el-button>
        </el-tooltip>
      </template>
    </PageHeader>

    <!-- Stats Bar -->
    <div class="stats-bar">
      <el-card class="stat-card" shadow="never">
        <div class="stat-value">{{ enabledCount }}/{{ totalCount }}</div>
        <div class="stat-label">{{ $t('experts.stats.active') }}</div>
      </el-card>
      <el-card class="stat-card" shadow="never">
        <div class="stat-value">{{ avgWeight }}%</div>
        <div class="stat-label">{{ $t('experts.stats.avgWeight') }}</div>
      </el-card>
      <el-card class="stat-card" shadow="never">
        <div class="stat-value">{{ totalCount }}</div>
        <div class="stat-label">{{ $t('experts.stats.total') }}</div>
      </el-card>
    </div>

    <!-- Filters -->
    <div class="filters-bar">
      <el-input
        v-model="searchQuery"
        :placeholder="$t('experts.searchPlaceholder')"
        clearable
        class="search-input"
      >
        <template #prefix>
          <el-icon><Search /></el-icon>
        </template>
      </el-input>
      <el-select v-model="filterCategory" :placeholder="$t('experts.categoryPlaceholder')" class="category-select">
        <el-option
          v-for="cat in categories"
          :key="cat.value"
          :label="cat.label"
          :value="cat.value"
        />
      </el-select>
    </div>

    <!-- Loading State -->
    <div v-if="loading" class="skeleton-grid">
      <el-skeleton
        v-for="i in 6"
        :key="i"
        animated
        class="skeleton-card"
      >
        <template #template>
          <div style="padding: 20px">
            <el-skeleton-item variant="circle" style="width: 40px; height: 40px; margin-bottom: var(--space-4)" />
            <el-skeleton-item variant="h3" style="width: 60%; margin-bottom: var(--space-3)" />
            <el-skeleton-item variant="text" style="width: 40%; margin-bottom: var(--space-4)" />
            <el-skeleton-item variant="p" style="width: 100%; margin-bottom: var(--space-2)" />
            <el-skeleton-item variant="p" style="width: 80%" />
          </div>
        </template>
      </el-skeleton>
    </div>

    <!-- Empty State -->
    <el-empty
      v-else-if="filteredExperts.length === 0"
      :description="$t('experts.empty')"
      :image-size="120"
    >
      <template #description>
        <p>{{ $t('experts.emptyFiltered') }}</p>
      </template>
      <el-button type="primary" @click="searchQuery = ''; filterCategory = 'all'">
        {{ $t('experts.clearFilters') }}
      </el-button>
    </el-empty>

    <!-- Expert Grid -->
    <div v-else class="experts-grid">
      <ExpertCard
        v-for="(expert, index) in filteredExperts"
        :key="expert.id"
        :expert="expert"
        :index="index"
        @toggle="handleToggle"
        @weight-change="handleWeightChange"
        @view-details="handleViewDetails"
      />
    </div>

    <!-- Detail Modal -->
    <el-dialog
      v-model="detailModalVisible"
      :title="$t('experts.detailsTitle')"
      width="var(--modal-w-lg)"
      class="expert-dialog"
      :aria-label="$t('experts.detailsAria')"
      destroy-on-close
    >
      <div v-if="selectedExpert" class="detail-content">
        <div class="detail-header">
          <h2 class="detail-name">{{ selectedExpert.name }}</h2>
          <el-tag size="small" effect="plain" class="category-tag">
            {{ categoryLabelMap[selectedExpert.category] }}
          </el-tag>
          <el-tag v-if="!selectedExpert.enabled" type="info" size="small" effect="plain">
            <el-icon><WarningFilled /></el-icon>
            {{ $t('common.disabled') }}
          </el-tag>
        </div>

        <el-divider />

        <div class="detail-section">
          <div class="detail-row">
            <span class="detail-label">{{ $t('experts.detail.enabled') }}</span>
            <el-switch
              :aria-label="$t('experts.detail.toggleAria', { name: selectedExpert.name })"
              :model-value="selectedExpert.enabled"
              @update:model-value="(val: boolean) => handleToggle(selectedExpert!.id, val)"
              :active-color="'var(--success)'"
              :inactive-color="'var(--offline)'"
            />
          </div>
          <div class="detail-row">
            <span class="detail-label">{{ $t('experts.detail.weight') }}</span>
            <div class="detail-value" style="flex: 1;">
              <el-slider
                :model-value="selectedExpert.weight"
                :max="100"
                :step="5"
                :show-stops="true"
                disabled
                style="width: 100%;"
              />
              <span class="weight-text">{{ selectedExpert.weight }}%</span>
            </div>
          </div>
        </div>

        <el-divider />

        <div class="detail-section">
          <h4 class="section-title">{{ $t('experts.detail.description') }}</h4>
          <el-input
            type="textarea"
            :model-value="selectedExpert.description"
            readonly
            :rows="3"
            resize="none"
          />
        </div>

        <div class="detail-section">
          <div class="prompt-header">
            <h4 class="section-title">{{ $t('experts.detail.prompt') }}</h4>
            <span
              v-if="promptSaveState === 'saved'"
              class="prompt-status"
              role="status"
            >{{ $t('experts.detail.promptSaved') }}</span>
          </div>
          <el-input
            :model-value="promptDraft"
            type="textarea"
            :rows="8"
            resize="none"
            class="prompt-textarea"
            :disabled="promptSaving"
            :maxlength="MAX_EXPERT_PROMPT_CHARS"
            :aria-label="$t('experts.detail.prompt')"
            @update:model-value="setPromptDraft"
          />
          <p class="prompt-source">
            {{ selectedExpert.promptOverride
              ? $t('experts.detail.promptSourceOverride')
              : $t('experts.detail.promptSourceConfig') }}
          </p>
          <div class="prompt-actions">
            <el-button
              type="primary"
              size="small"
              :loading="promptSaving"
              :disabled="!promptDirty || promptSaving"
              :aria-label="$t('experts.detail.promptSave')"
              @click="onSavePrompt"
            >
              {{ promptSaving ? $t('experts.detail.promptSaving') : $t('experts.detail.promptSave') }}
            </el-button>
          </div>
        </div>

        <div class="detail-section">
          <h4 class="section-title">{{ $t('experts.detail.lastReviews') }}</h4>
          <el-table
            :data="selectedExpert.lastReviews"
            size="small"
            class="reviews-table"
            @row-click="handleRowClick"
          >
            <el-table-column prop="mrTitle" :label="$t('history.columns.mrTitle')" min-width="180" show-overflow-tooltip />
            <el-table-column prop="score" :label="$t('experts.detail.score')" width="90" align="center">
              <template #default="{ row }">
                <el-tag
                  v-if="row.score !== undefined"
                  :type="getScoreType(row.score)"
                  size="small"
                  effect="plain"
                >
                  {{ row.score }}
                </el-tag>
                <span v-else class="text-muted">—</span>
              </template>
            </el-table-column>
            <el-table-column prop="date" :label="$t('experts.detail.date')" width="100" align="right" />
          </el-table>
        </div>
      </div>
      <template #footer>
        <el-button @click="detailModalVisible = false">
          <el-icon><Close /></el-icon>
          {{ $t('common.close') }}
        </el-button>
      </template>
    </el-dialog>
  </div>
</template>

<style scoped>
.experts-page {
  max-width: 1400px;
  margin: 0 auto;
}

.stats-bar {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: var(--space-4);
  margin-bottom: var(--space-5);
}

.stat-card {
  text-align: center;
  padding: var(--space-2);
  background-color: var(--bg-card);
  border-color: var(--border-color);
}

.stat-value {
  font-size: 28px;
  font-weight: 700;
  color: var(--brand);
  font-family: var(--font-mono);
  line-height: 1.2;
  margin-bottom: var(--space-1);
}

.stat-label {
  font-size: 13px;
  color: var(--text-secondary);
  font-weight: 500;
}

/* Filters */
.filters-bar {
  display: flex;
  gap: var(--space-3);
  margin-bottom: var(--space-5);
  flex-wrap: wrap;
}

.search-input {
  flex: 1;
  min-width: 200px;
}

.category-select {
  width: 180px;
  flex-shrink: 0;
}

/* Skeleton Grid */
.skeleton-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
  gap: var(--space-4);
}

.skeleton-card {
  background: var(--bg-card);
  border: 1px solid var(--border-color);
  border-radius: var(--radius-md);
  box-shadow: var(--shadow-card);
}

:deep(.skeleton-card .el-skeleton__item) {
  background: linear-gradient(90deg, var(--bg-surface) 25%, var(--bg-card) 50%, var(--bg-surface) 75%);
}

/* Expert Grid: `1fr` auto rows equalise every row's height, and the cards
   stretch to it, so all 12 cards line up and each "View Details" button sits
   on the same baseline (RENG-76 R0.3). */
.experts-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
  grid-auto-rows: 1fr;
  align-items: stretch;
  gap: var(--space-4);
}

/* Detail Dialog */
.detail-content {
  padding: 0 var(--space-1);
}

.detail-header {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
  margin-bottom: var(--space-2);
}

/* R1.5: the category is a muted chip — the nine-colour category palette is
   gone, so a card grid reads as one surface instead of a rainbow. */
.category-tag {
  background: var(--bg-hover);
  color: var(--text-secondary);
  border: none;
  font-weight: 500;
}

.detail-name {
  font-size: 20px;
  font-weight: 600;
  color: var(--text-primary);
  margin: 0;
  flex: 1;
  min-width: 0;
}

.detail-section {
  margin-bottom: 20px;
}

.detail-section:last-child {
  margin-bottom: 0;
}

.section-title {
  font-size: 14px;
  font-weight: 600;
  color: var(--text-primary);
  margin: 0 0 10px 0;
}

.detail-row {
  display: flex;
  align-items: center;
  gap: var(--space-4);
  margin-bottom: 10px;
}

.detail-label {
  font-size: 14px;
  color: var(--text-secondary);
  font-weight: 500;
  min-width: 70px;
}

.detail-value {
  font-size: 14px;
  color: var(--text-primary);
  font-weight: 600;
  font-family: var(--font-mono);
}

.prompt-textarea :deep(.el-textarea__inner) {
  font-family: var(--font-mono);
  font-size: 13px;
  line-height: 1.6;
  background-color: var(--bg-primary);
  color: var(--text-primary);
}

/* RENG-93: the editable prompt's header row (title + transient "Saved"), the
   source hint line, and the save action. All colours and spacing come off the
   token layer like the rest of the dialog. */
.prompt-header {
  display: flex;
  align-items: center;
  gap: var(--space-2);
}

.prompt-status {
  font-size: 12px;
  font-weight: 500;
  color: var(--accent-success);
}

.prompt-source {
  font-size: 12px;
  line-height: 1.5;
  color: var(--text-secondary);
  margin: var(--space-2) 0 0 0;
}

.prompt-actions {
  margin-top: var(--space-3);
}

.reviews-table {
  width: 100%;
}

.reviews-table :deep(.el-table__row) {
  cursor: pointer;
}

.reviews-table :deep(.el-table__row:hover) {
  background-color: var(--bg-hover);
}

.text-muted {
  color: var(--text-secondary);
  font-size: 13px;
}

/* Responsive */
@media (max-width: 768px) {
  :deep(.page-header) {
    flex-direction: column;
    align-items: stretch;
  }

  :deep(.page-header__right) {
    justify-content: flex-end;
  }

  .stats-bar {
    grid-template-columns: 1fr;
  }

  .filters-bar {
    flex-direction: column;
  }

  .search-input,
  .category-select {
    width: 100%;
  }

  .experts-grid {
    grid-template-columns: 1fr;
  }

  .skeleton-grid {
    grid-template-columns: 1fr;
  }
}

@media (min-width: 769px) and (max-width: 1023px) {
  .experts-grid {
    grid-template-columns: repeat(2, 1fr);
  }
  .skeleton-grid {
    grid-template-columns: repeat(2, 1fr);
  }
}

@media (min-width: 1024px) and (max-width: 1279px) {
  .experts-grid {
    grid-template-columns: repeat(3, 1fr);
  }
  .skeleton-grid {
    grid-template-columns: repeat(3, 1fr);
  }
}

@media (min-width: 1280px) {
  .experts-grid {
    grid-template-columns: repeat(4, 1fr);
  }
  .skeleton-grid {
    grid-template-columns: repeat(4, 1fr);
  }
}
</style>
