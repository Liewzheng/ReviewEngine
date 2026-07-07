<script setup lang="ts">
import { ref, computed, watch, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import {
  Search,
  Close,
  Refresh,
  ArrowRight,
  More,
  Download,
  Link,
  DocumentCopy,
  Tickets,
  Clock,
  User as UserIcon,
  Share,
  Document,
  Folder,
} from '@element-plus/icons-vue'
import { ElMessageBox, ElNotification } from 'element-plus'
import type { ReviewListItem, HistoryFilters } from '../types/history'
import { useReviews } from '../composables/useReviews'
import StatusBadge from '../components/ReviewHistory/StatusBadge.vue'

/* ─────────────── Router & Composable ─────────────── */
const route = useRoute()
const router = useRouter()
const reviews = useReviews()

const loading = reviews.loading
const drawerOpen = ref(false)
const selectedReview = reviews.selectedReview

const page = ref(1)
const pageSize = ref(25)

const filters = ref<HistoryFilters>({
  q: '',
  project: null,
  status: null,
  dateFrom: null,
  dateTo: null,
  repository: null,
})

const dateRange = computed({
  get: () => {
    const f = filters.value
    return f.dateFrom && f.dateTo ? [f.dateFrom, f.dateTo] : []
  },
  set: (val: string[]) => {
    if (val && val.length === 2) {
      filters.value.dateFrom = val[0]
      filters.value.dateTo = val[1]
    } else {
      filters.value.dateFrom = null
      filters.value.dateTo = null
    }
  },
})

/* ─────────────── Error Handling ─────────────── */
watch(() => reviews.error.value, (err) => {
  if (err) {
    ElNotification({
      title: 'Error',
      message: err,
      type: 'error',
      duration: 5000,
    })
  }
})

/* ─────────────── Data & Pagination ─────────────── */
const items = reviews.items
const total = reviews.total
const pagedReviews = items

const projects = computed(() => [...new Set(items.value.map(i => i.project).filter((p): p is string => !!p))])
const repositories = computed(() => [...new Set(items.value.map(i => i.repository).filter((r): r is string => !!r))])

async function fetchReviewsData() {
  await reviews.fetchReviews(filters.value, page.value, pageSize.value)
}

/* ─────────────── URL Sync ─────────────── */
function updateUrl() {
  const query: Record<string, string> = {}
  if (filters.value.q) query.q = filters.value.q
  if (filters.value.project) query.project = filters.value.project
  if (filters.value.status) query.status = filters.value.status
  if (filters.value.dateFrom) query.from = filters.value.dateFrom
  if (filters.value.dateTo) query.to = filters.value.dateTo
  if (filters.value.repository) query.repo = filters.value.repository
  if (page.value > 1) query.page = String(page.value)
  if (pageSize.value !== 25) query.size = String(pageSize.value)
  router.replace({ query })
}

function readUrl() {
  const q = route.query
  filters.value.q = (q.q as string) || ''
  filters.value.project = (q.project as string) || null
  filters.value.status = (q.status as string) || null
  filters.value.dateFrom = (q.from as string) || null
  filters.value.dateTo = (q.to as string) || null
  filters.value.repository = (q.repo as string) || null
  page.value = q.page ? parseInt(q.page as string, 10) : 1
  pageSize.value = q.size ? parseInt(q.size as string, 10) : 25
}

/* ─────────────── Debounce ─────────────── */
let searchTimeout: ReturnType<typeof setTimeout>
function onSearchInput() {
  clearTimeout(searchTimeout)
  searchTimeout = setTimeout(() => {
    page.value = 1
    updateUrl()
    fetchReviewsData()
  }, 300)
}

function onFilterChange() {
  page.value = 1
  updateUrl()
  fetchReviewsData()
}

function resetFilters() {
  filters.value = {
    q: '',
    project: null,
    status: null,
    dateFrom: null,
    dateTo: null,
    repository: null,
  }
  page.value = 1
  pageSize.value = 25
  updateUrl()
  fetchReviewsData()
}

/* ─────────────── Drawer ─────────────── */
async function openDrawer(row: ReviewListItem) {
  await reviews.fetchReview(row.id)
  if (reviews.selectedReview.value) {
    drawerOpen.value = true
  }
}

/* ─────────────── Actions ─────────────── */
function handleRerun(row: ReviewListItem) {
  ElMessageBox.confirm(
    `Re-run review for "${row.mrTitle}"? This will post a new comment to the MR.`,
    'Re-run Review',
    { confirmButtonText: 'Re-run', cancelButtonText: 'Cancel', type: 'warning' }
  ).then(() => {
    reviews.rerun(row.id).then(() => {
      ElNotification.success({ title: 'Review re-queued', message: `A new review has been queued for ${row.mrTitle}.` })
      fetchReviewsData()
    })
  }).catch(() => {})
}

function copyReviewId(id: string) {
  navigator.clipboard.writeText(id).then(() => {
    ElNotification.success({ title: 'Copied', message: `Review ID ${id} copied to clipboard.` })
  }).catch(() => {
    ElNotification.warning({ title: 'Copy failed', message: 'Could not copy to clipboard.' })
  })
}

function viewLogs(row: ReviewListItem) {
  router.push(`/logs?reviewId=${row.id}`)
}

function viewOriginalComment(row: ReviewListItem) {
  if (row.gitlabMrUrl) {
    window.open(row.gitlabMrUrl, '_blank')
  } else {
    ElNotification.warning({ title: 'Unavailable', message: 'Original comment URL not available.' })
  }
}

/* ─────────────── Formatting ─────────────── */
const headerCellStyle = {
  background: '#f5f7fa',
  color: '#303133',
  fontWeight: 600,
  fontSize: '12px',
  textTransform: 'uppercase' as const,
  letterSpacing: '0.05em',
}

function formatDuration(ms: number): string {
  if (ms <= 0) return '-'
  const sec = Math.floor(ms / 1000)
  if (sec < 60) return `${sec}s`
  const min = Math.floor(sec / 60)
  const rem = sec % 60
  return `${min}m ${rem}s`
}

function formatRelativeTime(iso: string): string {
  const d = new Date(iso)
  const now = new Date()
  const diffSec = Math.floor((now.getTime() - d.getTime()) / 1000)
  if (diffSec < 60) return 'just now'
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m ago`
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h ago`
  if (diffSec < 604800) return `${Math.floor(diffSec / 86400)}d ago`
  return d.toLocaleDateString()
}

function formatDate(iso: string): string {
  return new Date(iso).toLocaleString()
}

function getInitials(name: string): string {
  return name
    .split(' ')
    .map((n) => n[0])
    .join('')
    .toUpperCase()
    .slice(0, 2)
}

/* ─────────────── Pagination display ─────────────── */
const paginationInfo = computed(() => {
  if (total.value === 0) return 'Showing 0 reviews'
  const start = (page.value - 1) * pageSize.value + 1
  const end = Math.min(page.value * pageSize.value, total.value)
  return `Showing ${start} to ${end} of ${total.value} reviews`
})

/* ─────────────── Init ─────────────── */
onMounted(() => {
  readUrl()
  fetchReviewsData()
})

watch(() => route.query, () => {
  readUrl()
  fetchReviewsData()
}, { deep: true })
</script>

<template>
  <div class="history-page">
    <!-- Header -->
    <div class="page-header">
      <h2 class="page-title">Review History</h2>
      <el-button type="primary" :icon="Download" plain aria-label="Export reviews">
        Export
      </el-button>
    </div>

    <!-- Filter Bar -->
    <div class="filter-bar">
      <el-input
        v-model="filters.q"
        placeholder="Search MR title, author, branch..."
        :prefix-icon="Search"
        clearable
        @input="onSearchInput"
        @clear="onSearchInput"
        class="filter-search"
      />
      <el-select
        v-model="filters.project"
        placeholder="All Projects"
        clearable
        @change="onFilterChange"
        class="filter-select"
      >
        <el-option
          v-for="p in projects"
          :key="p"
          :label="p"
          :value="p"
        />
      </el-select>
      <el-select
        v-model="filters.status"
        placeholder="All Statuses"
        clearable
        @change="onFilterChange"
        class="filter-select"
      >
        <el-option label="Queued" value="queued" />
        <el-option label="Running" value="running" />
        <el-option label="Completed" value="completed" />
        <el-option label="Failed" value="failed" />
        <el-option label="Cancelled" value="cancelled" />
      </el-select>
      <el-date-picker
        v-model="dateRange"
        type="daterange"
        range-separator="to"
        start-placeholder="Start"
        end-placeholder="End"
        format="YYYY-MM-DD"
        value-format="YYYY-MM-DD"
        @change="onFilterChange"
        class="filter-date"
      />
      <el-select
        v-model="filters.repository"
        placeholder="All Repositories"
        clearable
        @change="onFilterChange"
        class="filter-select"
      >
        <el-option
          v-for="r in repositories"
          :key="r"
          :label="r"
          :value="r"
        />
      </el-select>
      <el-button :icon="Close" @click="resetFilters" aria-label="Reset filters">
        Reset
      </el-button>
    </div>

    <!-- Loading Skeleton -->
    <div v-if="loading" class="skeleton-wrapper">
      <el-skeleton :rows="5" animated />
    </div>

    <!-- Empty State -->
    <el-empty v-else-if="total === 0" description="No reviews found" />

    <!-- Data Table -->
    <template v-else>
      <el-card class="table-card">
        <el-table
          :data="pagedReviews"
          style="width: 100%"
          :header-cell-style="headerCellStyle"
          @row-click="(row: ReviewListItem) => openDrawer(row)"
          class="history-table"
          :height="'calc(100vh - 280px)'"
          :stripe="false"
          :border="false"
          :highlight-current-row="false"
        >
          <el-table-column label="MR Title" min-width="240" sortable :sort-by="['mrTitle']">
            <template #default="{ row }">
              <div class="title-cell">
                <el-tag size="small" type="info" class="project-tag">{{ row.project }}</el-tag>
                <div class="title-text">
                  <div class="mr-title">{{ row.mrTitle }}</div>
                  <div class="branch-name">
                    <el-icon><Share /></el-icon>
                    {{ row.branch }} &rarr; {{ row.targetBranch }}
                  </div>
                </div>
              </div>
            </template>
          </el-table-column>

          <el-table-column prop="project" label="Project" width="140" sortable class-name="col-project">
            <template #default="{ row }">
              <el-tag size="small" type="info">{{ row.project }}</el-tag>
            </template>
          </el-table-column>

          <el-table-column label="Author" width="160" sortable :sort-by="['author.name']">
            <template #default="{ row }">
              <div class="author-cell">
                <div class="author-avatar">
                  <img v-if="row.author.avatarUrl" :src="row.author.avatarUrl" alt="" />
                  <span v-else>{{ getInitials(row.author.name) }}</span>
                </div>
                <span class="author-name">{{ row.author.name }}</span>
              </div>
            </template>
          </el-table-column>

          <el-table-column label="Status" width="120" sortable :sort-by="['status']">
            <template #default="{ row }">
              <StatusBadge :status="row.status" size="small" />
            </template>
          </el-table-column>

          <el-table-column label="Duration" width="100" sortable :sort-by="['durationMs']">
            <template #default="{ row }">
              <span class="duration-text">{{ formatDuration(row.durationMs) }}</span>
            </template>
          </el-table-column>

          <el-table-column label="Created" width="150" sortable :sort-by="['createdAt']">
            <template #default="{ row }">
              <el-tooltip :content="formatDate(row.createdAt)" placement="top">
                <span class="created-text">{{ formatRelativeTime(row.createdAt) }}</span>
              </el-tooltip>
            </template>
          </el-table-column>

          <el-table-column label="Actions" width="140" fixed="right">
            <template #default="{ row }">
              <el-button-group class="actions-group">
                <el-tooltip content="Re-run review">
                  <el-button size="small" :icon="Refresh" @click.stop="handleRerun(row)" aria-label="Re-run review" />
                </el-tooltip>
                <el-tooltip content="View details">
                  <el-button size="small" :icon="ArrowRight" @click.stop="openDrawer(row)" aria-label="View details" />
                </el-tooltip>
                <el-dropdown trigger="click" @command="(cmd: string) => {
                  if (cmd === 'comment') viewOriginalComment(row)
                  if (cmd === 'copy') copyReviewId(row.id)
                  if (cmd === 'logs') viewLogs(row)
                }">
                  <el-button size="small" :icon="More" @click.stop aria-label="More actions" />
                  <template #dropdown>
                    <el-dropdown-menu>
                      <el-dropdown-item command="comment" :icon="Link">View original comment</el-dropdown-item>
                      <el-dropdown-item command="copy" :icon="DocumentCopy">Copy review ID</el-dropdown-item>
                      <el-dropdown-item command="logs" :icon="Tickets">View logs</el-dropdown-item>
                    </el-dropdown-menu>
                  </template>
                </el-dropdown>
              </el-button-group>
            </template>
          </el-table-column>
        </el-table>
      </el-card>

      <!-- Pagination -->
      <div class="pagination-bar">
        <span class="pagination-info">{{ paginationInfo }}</span>
        <div class="pagination-controls">
          <el-select v-model="pageSize" @change="() => { page = 1; updateUrl() }" style="width: 100px">
            <el-option label="25 / page" :value="25" />
            <el-option label="50 / page" :value="50" />
            <el-option label="100 / page" :value="100" />
          </el-select>
          <el-pagination
            v-model:current-page="page"
            v-model:page-size="pageSize"
            :total="total"
            layout="total, prev, pager, next, jumper"
            @change="updateUrl"
          />
        </div>
      </div>
    </template>

    <!-- Detail Drawer -->
    <el-drawer
      v-model="drawerOpen"
      size="600px"
      class="detail-drawer"
    >
      <template #header>
        <div class="drawer-title-row">
          <h3 class="drawer-title">{{ selectedReview?.mrTitle || 'Review Details' }}</h3>
          <StatusBadge v-if="selectedReview" :status="selectedReview.status" />
        </div>
      </template>
      <div v-if="selectedReview" class="drawer-content">
        <!-- Meta Grid -->
        <div class="meta-grid">
          <div class="meta-item">
            <el-icon><UserIcon /></el-icon>
            <div>
              <div class="meta-label">Author</div>
              <div class="meta-value">{{ selectedReview.author.name }}</div>
            </div>
          </div>
          <div class="meta-item">
            <el-icon><Folder /></el-icon>
            <div>
              <div class="meta-label">Project</div>
              <div class="meta-value">{{ selectedReview.project }}</div>
            </div>
          </div>
          <div class="meta-item">
            <el-icon><Link /></el-icon>
            <div>
              <div class="meta-label">Branch</div>
              <div class="meta-value">{{ selectedReview.branch }}</div>
            </div>
          </div>
          <div class="meta-item">
            <el-icon><Clock /></el-icon>
            <div>
              <div class="meta-label">Created</div>
              <div class="meta-value">{{ formatDate(selectedReview.createdAt) }}</div>
            </div>
          </div>
          <div class="meta-item">
            <el-icon><Clock /></el-icon>
            <div>
              <div class="meta-label">Duration</div>
              <div class="meta-value">{{ formatDuration(selectedReview.durationMs) }}</div>
            </div>
          </div>
          <div class="meta-item">
            <el-icon><Document /></el-icon>
            <div>
              <div class="meta-label">Commit</div>
              <div class="meta-value mono">{{ selectedReview.commitSha }}</div>
            </div>
          </div>
        </div>

        <el-divider />

        <!-- Expert Results -->
        <h4 class="drawer-section-title">Expert Results</h4>
        <el-collapse>
          <el-collapse-item
            v-for="exp in selectedReview.experts"
            :key="exp.expertId"
            :title="exp.expertName"
          >
            <template #title>
              <div class="expert-title">
                <span>{{ exp.expertName }}</span>
                <div class="expert-meta">
                  <StatusBadge :status="exp.status" size="small" />
                  <el-tag v-if="exp.score" size="small" :type="exp.score >= 80 ? 'success' : exp.score >= 60 ? 'warning' : 'danger'">
                    {{ exp.score }}
                  </el-tag>
                </div>
              </div>
            </template>
            <div class="expert-content">
              <p class="expert-summary">{{ exp.summary }}</p>
              <p v-if="exp.details" class="expert-details">{{ exp.details }}</p>
            </div>
          </el-collapse-item>
        </el-collapse>

        <el-divider />

        <!-- Raw Data Tabs -->
        <el-tabs>
          <el-tab-pane label="Summary">
            <div class="raw-panel">
              <p class="raw-text">Review generated by {{ selectedReview.experts.length }} experts with overall status <StatusBadge :status="selectedReview.status" size="small" />.</p>
            </div>
          </el-tab-pane>
          <el-tab-pane label="Full Comment">
            <div class="raw-panel">
              <el-input
                v-model="selectedReview.rawComment"
                type="textarea"
                :rows="10"
                readonly
                resize="none"
              />
            </div>
          </el-tab-pane>
          <el-tab-pane label="API Response">
            <div class="raw-panel">
              <pre class="json-block">{{ JSON.stringify(selectedReview.rawApiResponse, null, 2) }}</pre>
            </div>
          </el-tab-pane>
        </el-tabs>

        <!-- Footer Actions -->
        <div class="drawer-footer">
          <el-button type="primary" :icon="Refresh" @click="handleRerun(selectedReview)">
            Re-run Review
          </el-button>
          <el-button :icon="Link" @click="viewOriginalComment(selectedReview)">
            View on GitLab
          </el-button>
          <el-button @click="drawerOpen = false">
            Close
          </el-button>
        </div>
      </div>
    </el-drawer>
  </div>
</template>

<style scoped>
.history-page {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.page-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.page-title {
  font-size: 24px;
  font-weight: 600;
  letter-spacing: -0.02em;
  line-height: 1.3;
  color: var(--text-primary);
  margin: 0;
}

/* Filter Bar */
.filter-bar {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  align-items: center;
  padding: 16px 20px;
  background: var(--bg-card);
  border: 1px solid var(--border-color);
  border-radius: var(--radius-md);
}

.filter-search {
  min-width: 280px;
  flex: 1;
}

.filter-select {
  width: 160px;
}

.filter-date {
  width: 260px;
}

/* Table */
.table-card {
  overflow: hidden;
}

.history-table :deep(.el-table__row) {
  cursor: pointer;
}

.history-table :deep(.el-table__row:hover) {
  background: var(--bg-hover) !important;
}

.title-cell {
  display: flex;
  align-items: flex-start;
  gap: 8px;
}

.project-tag {
  flex-shrink: 0;
  margin-top: 2px;
}

.title-text {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.mr-title {
  font-size: 13px;
  color: var(--text-primary);
  font-weight: 500;
  line-height: 1.4;
}

.branch-name {
  font-size: 11px;
  color: var(--text-secondary);
  font-family: var(--font-mono);
  display: flex;
  align-items: center;
  gap: 4px;
}

.branch-name .el-icon {
  font-size: 11px;
}

.author-cell {
  display: flex;
  align-items: center;
  gap: 8px;
}

.author-avatar {
  width: 28px;
  height: 28px;
  border-radius: 50%;
  background: var(--brand);
  color: white;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 11px;
  font-weight: 600;
  flex-shrink: 0;
}

.author-avatar img {
  width: 100%;
  height: 100%;
  border-radius: 50%;
  object-fit: cover;
}

.author-name {
  font-size: 13px;
  color: var(--text-primary);
}

.duration-text {
  font-family: var(--font-mono);
  font-size: 13px;
  color: var(--text-secondary);
}

.created-text {
  font-size: 13px;
  color: var(--text-secondary);
}

.actions-group {
  display: flex;
}

/* Pagination */
.pagination-bar {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 12px 0;
  flex-wrap: wrap;
  gap: 12px;
}

.pagination-info {
  font-size: 12px;
  color: var(--text-secondary);
}

.pagination-controls {
  display: flex;
  align-items: center;
  gap: 12px;
}

/* Skeleton */
.skeleton-wrapper {
  padding: 20px;
  background: var(--bg-card);
  border-radius: var(--radius-md);
  border: 1px solid var(--border-color);
}

/* Drawer */
.drawer-content {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.drawer-title-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  width: 100%;
}

.drawer-title {
  font-size: 16px;
  font-weight: 600;
  color: var(--text-primary);
  margin: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.drawer-header {
  display: flex;
  justify-content: flex-end;
}

.meta-grid {
  display: grid;
  grid-template-columns: repeat(2, 1fr);
  gap: 12px;
}

.meta-item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 12px;
  background: var(--bg-surface);
  border-radius: var(--radius-md);
  border: 1px solid var(--border-color);
}

.meta-item .el-icon {
  color: var(--text-secondary);
  font-size: 16px;
}

.meta-label {
  font-size: 11px;
  color: var(--text-secondary);
  text-transform: uppercase;
  letter-spacing: 0.5px;
}

.meta-value {
  font-size: 13px;
  color: var(--text-primary);
  font-weight: 500;
  word-break: break-all;
}

.meta-value.mono {
  font-family: var(--font-mono);
  font-size: 12px;
}

.drawer-section-title {
  font-size: 14px;
  font-weight: 600;
  color: var(--text-primary);
  margin: 0 0 8px;
}

.expert-title {
  display: flex;
  justify-content: space-between;
  align-items: center;
  width: 100%;
  padding-right: 24px;
  font-size: 13px;
  color: var(--text-primary);
}

.expert-meta {
  display: flex;
  align-items: center;
  gap: 8px;
}

.expert-content {
  padding: 8px 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.expert-summary {
  font-size: 13px;
  color: var(--text-primary);
  line-height: 1.5;
  margin: 0;
}

.expert-details {
  font-size: 12px;
  color: var(--text-secondary);
  line-height: 1.5;
  margin: 0;
  padding: 8px;
  background: var(--bg-surface);
  border-radius: var(--radius-sm);
  border: 1px solid var(--border-color);
  font-family: var(--font-sans);
}

.raw-panel {
  padding: 8px 0;
}

.raw-text {
  font-size: 13px;
  color: var(--text-primary);
  line-height: 1.6;
}

.json-block {
  background: var(--bg-surface);
  border: 1px solid var(--border-color);
  border-radius: var(--radius-md);
  padding: 12px;
  font-family: var(--font-mono);
  font-size: 12px;
  color: var(--text-primary);
  overflow-x: auto;
  max-height: 400px;
  overflow-y: auto;
  margin: 0;
}

.drawer-footer {
  display: flex;
  gap: 8px;
  padding-top: 16px;
  border-top: 1px solid var(--border-color);
  margin-top: auto;
  flex-wrap: wrap;
}

/* Responsive */
@media (max-width: 1024px) {
  .filter-bar {
    flex-direction: column;
    align-items: stretch;
  }

  .filter-search,
  .filter-select,
  .filter-date {
    width: 100%;
    min-width: unset;
  }

  .history-table :deep(.col-project),
  .history-table :deep(.col-repository) {
    display: none;
  }

  .meta-grid {
    grid-template-columns: 1fr;
  }
}

@media (max-width: 768px) {
  .page-header {
    flex-direction: column;
    align-items: flex-start;
    gap: 12px;
  }

  .history-table :deep(.el-table__cell:not(.el-table-column--selection):not(.is-fixed-right)) {
    display: none;
  }

  .history-table :deep(.el-table__cell:first-child),
  .history-table :deep(.el-table__cell:nth-child(4)),
  .history-table :deep(.is-fixed-right) {
    display: table-cell;
  }

  .pagination-bar {
    flex-direction: column;
    align-items: flex-start;
  }

  .pagination-controls {
    width: 100%;
    justify-content: space-between;
  }

  .drawer-footer {
    flex-direction: column;
  }

  .drawer-footer .el-button {
    width: 100%;
  }
}
</style>
