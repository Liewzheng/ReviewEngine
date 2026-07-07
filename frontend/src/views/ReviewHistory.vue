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
import type {
  ReviewListItem,
  ReviewDetail,
  HistoryFilters,
  ReviewStatus,
} from '../types/history'
import StatusBadge from '../components/ReviewHistory/StatusBadge.vue'

/* ─────────────── Router & State ─────────────── */
const route = useRoute()
const router = useRouter()

const loading = ref(false)
const drawerOpen = ref(false)
const selectedReview = ref<ReviewDetail | null>(null)

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

/* ─────────────── Mock Data ─────────────── */
const projects = ['frontend', 'backend', 'mobile-app', 'api-gateway', 'docs']
const repositories = ['review-engine', 'dashboard', 'mobile-sdk', 'auth-service', 'core-api']
const authors = [
  { name: 'Alice Chen', avatarUrl: '' },
  { name: 'Bob Smith', avatarUrl: '' },
  { name: 'Carol Jones', avatarUrl: '' },
  { name: 'David Lee', avatarUrl: '' },
  { name: 'Eve Wang', avatarUrl: '' },
  { name: 'Frank Zhao', avatarUrl: '' },
  { name: 'Grace Liu', avatarUrl: '' },
]

const mrTitles = [
  'feat: add OAuth2 login support',
  'fix: resolve memory leak in worker pool',
  'refactor: simplify review queue dispatcher',
  'feat: implement dark mode toggle',
  'chore: upgrade Element Plus to v2.15',
  'fix: correct pagination offset on page resize',
  'docs: update API reference for v3',
  'feat: add export to CSV for history page',
  'test: add e2e coverage for review flow',
  'fix: handle null pointer in expert parser',
  'refactor: migrate Pinia stores to v3',
  'feat: support multi-tenant project isolation',
  'fix: GitLab webhook signature verification',
  'chore: update ESLint config and fix warnings',
  'feat: introduce pluggable expert system',
  'fix: retry logic for LLM rate limiting',
  'docs: add architecture decision records',
  'perf: reduce bundle size by 40%',
  'feat: add real-time queue monitoring',
  'fix: CSS variable fallback in light mode',
  'refactor: consolidate TypeScript types',
  'feat: implement review history search',
  'fix: drawer overflow on mobile screens',
  'test: mock data helpers for unit tests',
  'chore: configure Dependabot for frontend',
  'feat: add review detail drawer',
  'fix: correct status badge color mapping',
  'docs: add setup instructions for new devs',
  'perf: optimize table rendering for large lists',
  'feat: support GitHub PR reviews',
  'fix: timezone handling in date pickers',
  'refactor: extract reusable status components',
  'chore: clean up dead code in dashboard',
  'feat: add bulk re-review action',
  'fix: websocket reconnect logic',
  'test: snapshot tests for UI components',
  'docs: document environment variables',
  'feat: implement notification preferences',
  'fix: race condition in concurrent reviews',
  'perf: cache project list for 5 minutes',
  'feat: add keyboard shortcuts for navigation',
  'fix: broken link in error page',
  'chore: update Docker base image',
  'test: add contract tests for LLM providers',
  'feat: support custom expert templates',
  'fix: correct pluralization in status labels',
  'docs: add troubleshooting guide',
  'perf: lazy load chart components',
  'feat: add user profile settings',
  'fix: sidebar collapse animation jitter',
]

function randomDate(daysBack: number): string {
  const d = new Date()
  d.setDate(d.getDate() - Math.floor(Math.random() * daysBack))
  d.setHours(Math.floor(Math.random() * 24), Math.floor(Math.random() * 60))
  return d.toISOString()
}

function randomItem<T>(arr: T[]): T {
  return arr[Math.floor(Math.random() * arr.length)]
}

function randomStatus(): ReviewStatus {
  const weights: ReviewStatus[] = ['completed', 'completed', 'completed', 'completed', 'running', 'queued', 'failed', 'failed', 'cancelled']
  return randomItem(weights)
}

const allReviews = ref<ReviewListItem[]>([])

function generateMockData() {
  const data: ReviewListItem[] = []
  for (let i = 0; i < 120; i++) {
    const status = randomStatus()
    const project = randomItem(projects)
    const repo = randomItem(repositories)
    const author = randomItem(authors)
    const mrTitle = randomItem(mrTitles)
    const branch = `feature/${mrTitle.split(':')[0].replace(/\s/g, '-')}-${Math.floor(Math.random() * 100)}`
    const createdAt = randomDate(30)
    const durationMs = status === 'running' || status === 'queued' ? 0 : Math.floor(Math.random() * 300000) + 5000

    data.push({
      id: `rev-${1000 + i}`,
      mrTitle,
      project,
      repository: repo,
      branch,
      targetBranch: 'main',
      author,
      status,
      durationMs,
      createdAt,
      gitlabMrUrl: `https://gitlab.example.com/${repo}/-/merge_requests/${i + 1}`,
    })
  }
  // Sort by createdAt descending
  data.sort((a, b) => new Date(b.createdAt).getTime() - new Date(a.createdAt).getTime())
  allReviews.value = data
}

/* ─────────────── Filtering ─────────────── */
const filteredReviews = computed(() => {
  let result = [...allReviews.value]

  if (filters.value.q) {
    const q = filters.value.q.toLowerCase()
    result = result.filter(
      (r) =>
        r.mrTitle.toLowerCase().includes(q) ||
        r.author.name.toLowerCase().includes(q) ||
        r.branch.toLowerCase().includes(q) ||
        r.project.toLowerCase().includes(q)
    )
  }

  if (filters.value.project) {
    result = result.filter((r) => r.project === filters.value.project)
  }

  if (filters.value.status) {
    result = result.filter((r) => r.status === filters.value.status)
  }

  if (filters.value.repository) {
    result = result.filter((r) => r.repository === filters.value.repository)
  }

  if (filters.value.dateFrom) {
    const from = new Date(filters.value.dateFrom)
    result = result.filter((r) => new Date(r.createdAt) >= from)
  }

  if (filters.value.dateTo) {
    const to = new Date(filters.value.dateTo)
    to.setHours(23, 59, 59, 999)
    result = result.filter((r) => new Date(r.createdAt) <= to)
  }

  return result
})

const total = computed(() => filteredReviews.value.length)

const pagedReviews = computed(() => {
  const start = (page.value - 1) * pageSize.value
  return filteredReviews.value.slice(start, start + pageSize.value)
})

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
  }, 300)
}

function onFilterChange() {
  page.value = 1
  updateUrl()
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
}

/* ─────────────── Drawer ─────────────── */
function openDrawer(row: ReviewListItem) {
  const detail: ReviewDetail = {
    ...row,
    completedAt: row.status === 'completed' || row.status === 'failed' || row.status === 'cancelled'
      ? new Date(new Date(row.createdAt).getTime() + row.durationMs).toISOString()
      : undefined,
    commitSha: Array.from({ length: 8 }, () => '0123456789abcdef'[Math.floor(Math.random() * 16)]).join(''),
    experts: [
      {
        expertId: 'exp-1',
        expertName: 'Code Quality Expert',
        status: randomItem(['success', 'warning', 'error']),
        score: Math.floor(Math.random() * 40) + 60,
        summary: 'Code follows most style guidelines. A few minor issues with variable naming and missing JSDoc comments.',
        details: 'Detailed analysis shows 3 warnings in `src/utils/parser.ts` and 1 error in `src/components/Form.vue`.',
      },
      {
        expertId: 'exp-2',
        expertName: 'Security Expert',
        status: randomItem(['success', 'warning', 'success']),
        score: Math.floor(Math.random() * 30) + 70,
        summary: 'No critical security vulnerabilities detected. One potential XSS vector in user-generated content rendering.',
        details: 'The `v-html` directive in `RichText.vue` should be sanitized using DOMPurify before rendering.',
      },
      {
        expertId: 'exp-3',
        expertName: 'Performance Expert',
        status: randomItem(['success', 'warning', 'error', 'skipped']),
        score: Math.floor(Math.random() * 50) + 50,
        summary: 'Bundle size increased by 12%. Consider code-splitting the chart components.',
        details: 'The main bundle now includes `echarts` and `d3` which are only used in the dashboard. Use dynamic imports.',
      },
      {
        expertId: 'exp-4',
        expertName: 'Accessibility Expert',
        status: randomItem(['success', 'success', 'warning']),
        score: Math.floor(Math.random() * 25) + 75,
        summary: 'Good ARIA usage overall. Missing `aria-label` on 2 icon-only buttons.',
        details: 'Buttons in `Toolbar.vue` (lines 45 and 78) need aria-labels for screen reader compatibility.',
      },
    ],
    rawComment: `## Review Summary\n\nOverall the changes look good. There are a few areas that could use improvement:\n\n1. **Code Quality**: Minor style inconsistencies in the new components.\n2. **Security**: Potential XSS vector needs addressing.\n3. **Performance**: Consider lazy loading heavy dependencies.\n4. **Accessibility**: Add aria-labels to icon-only buttons.\n\nPlease address the security concern before merging.`,
    rawApiResponse: {
      reviewId: row.id,
      mrTitle: row.mrTitle,
      status: row.status,
      experts: ['exp-1', 'exp-2', 'exp-3', 'exp-4'],
      metadata: {
        modelVersion: 'gpt-4o-2024-05-13',
        tokensUsed: 3421,
        costUsd: 0.042,
        latencyMs: 1250,
      },
    },
  }
  selectedReview.value = detail
  drawerOpen.value = true
}

/* ─────────────── Actions ─────────────── */
function handleRerun(row: ReviewListItem) {
  ElMessageBox.confirm(
    `Re-run review for "${row.mrTitle}"? This will post a new comment to the MR.`,
    'Re-run Review',
    { confirmButtonText: 'Re-run', cancelButtonText: 'Cancel', type: 'warning' }
  ).then(() => {
    ElNotification.success({ title: 'Review re-queued', message: `A new review has been queued for ${row.mrTitle}.` })
    const idx = allReviews.value.findIndex((r) => r.id === row.id)
    if (idx !== -1) {
      allReviews.value[idx] = { ...allReviews.value[idx], status: 'queued', durationMs: 0 }
    }
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
  loading.value = true
  readUrl()
  generateMockData()
  setTimeout(() => {
    loading.value = false
  }, 600)
})

watch(() => route.query, readUrl, { deep: true })
</script>

<template>
  <div class="history-page">
    <!-- Header -->
    <div class="page-header">
      <h2 class="page-title">Review History</h2>
      <el-button type="primary" :icon="Download" plain>
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
      <el-button :icon="Close" @click="resetFilters">
        Reset
      </el-button>
    </div>

    <!-- Loading Skeleton -->
    <div v-if="loading" class="skeleton-wrapper">
      <el-skeleton :rows="10" animated />
    </div>

    <!-- Empty State -->
    <el-empty v-else-if="total === 0" description="No reviews found" />

    <!-- Data Table -->
    <template v-else>
      <el-card class="table-card">
        <el-table
          :data="pagedReviews"
          style="width: 100%"
          :header-cell-style="{ fontWeight: 600 }"
          @row-click="(row: ReviewListItem) => openDrawer(row)"
          class="history-table"
          :height="'calc(100vh - 320px)'"
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

          <el-table-column prop="project" label="Project" width="140" sortable class-name="col-project" />

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
                  <el-button size="small" :icon="Refresh" @click.stop="handleRerun(row)" />
                </el-tooltip>
                <el-tooltip content="View details">
                  <el-button size="small" :icon="ArrowRight" @click.stop="openDrawer(row)" />
                </el-tooltip>
                <el-dropdown trigger="click" @command="(cmd: string) => {
                  if (cmd === 'comment') viewOriginalComment(row)
                  if (cmd === 'copy') copyReviewId(row.id)
                  if (cmd === 'logs') viewLogs(row)
                }">
                  <el-button size="small" :icon="More" @click.stop />
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
            layout="prev, pager, next"
            @change="updateUrl"
          />
        </div>
      </div>
    </template>

    <!-- Detail Drawer -->
    <el-drawer
      v-model="drawerOpen"
      :title="selectedReview?.mrTitle || 'Review Details'"
      size="600px"
      class="detail-drawer"
    >
      <div v-if="selectedReview" class="drawer-content">
        <!-- Header -->
        <div class="drawer-header">
          <StatusBadge :status="selectedReview.status" />
        </div>

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
            <el-icon><GitBranch /></el-icon>
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
  font-size: 20px;
  font-weight: 600;
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

  .history-table :deep(.col-project) {
    display: none;
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
