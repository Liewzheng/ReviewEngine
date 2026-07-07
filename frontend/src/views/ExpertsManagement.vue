<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount } from 'vue'
import { useRouter } from 'vue-router'
import {
  Plus,
  Edit as IconEdit,
  Check,
  Close,
  Search,
  WarningFilled,
} from '@element-plus/icons-vue'
import { ElNotification } from 'element-plus'
import type { Expert, ExpertCategory, ExpertReviewSummary } from '../types/expert'
import { categoryColorMap, categoryLabelMap } from '../types/expert'
import ExpertCard from '../components/ExpertsManagement/ExpertCard.vue'

const router = useRouter()

// ========== State ==========
const experts = ref<Expert[]>([])
const loading = ref(false)
const isEditing = ref(false)
const detailModalVisible = ref(false)
const editModalVisible = ref(false)
const selectedExpert = ref<Expert | null>(null)
const editingExpert = ref<Expert | null>(null)
const searchQuery = ref('')
const filterCategory = ref<ExpertCategory | 'all'>('all')

// ========== Mock Data ==========
const mockExperts: Expert[] = [
  {
    id: 'expert-001',
    name: 'Security Sentinel',
    category: 'security',
    icon: 'Lock',
    enabled: true,
    weight: 85,
    description: 'Reviews security vulnerabilities, injection risks, secret leaks, and insecure dependencies. Ensures compliance with OWASP guidelines and best practices.',
    promptPreview: `You are a security-focused code review expert. Analyze the provided code for:
- SQL injection, XSS, and command injection vulnerabilities
- Hardcoded secrets, API keys, and tokens
- Insecure dependencies and outdated packages
- Missing authentication/authorization checks
- Unsafe deserialization or file operations

Provide severity ratings and actionable remediation steps.`,
    lastReviews: [
      { reviewId: 'rev-101', mrTitle: 'Fix authentication middleware', score: 92, date: '2024-01-15' },
      { reviewId: 'rev-102', mrTitle: 'Update JWT handling', score: 78, date: '2024-01-14' },
      { reviewId: 'rev-103', mrTitle: 'Add rate limiting', score: 88, date: '2024-01-12' },
      { reviewId: 'rev-104', mrTitle: 'Secure file upload', score: 95, date: '2024-01-10' },
      { reviewId: 'rev-105', mrTitle: 'Refactor password hashing', score: 90, date: '2024-01-08' },
    ],
  },
  {
    id: 'expert-002',
    name: 'Performance Hawk',
    category: 'performance',
    icon: 'Lightning',
    enabled: true,
    weight: 75,
    description: 'Identifies performance bottlenecks, memory leaks, inefficient algorithms, and suboptimal database queries. Recommends caching strategies.',
    promptPreview: `You are a performance optimization expert. Analyze code for:
- Algorithmic complexity (O(n²) loops, recursive explosions)
- Memory leaks and unnecessary allocations
- N+1 database queries and missing indexes
- Synchronous blocking operations in async contexts
- Inefficient caching patterns
- Large bundle sizes and lazy loading opportunities

Rate impact severity and suggest concrete optimizations.`,
    lastReviews: [
      { reviewId: 'rev-201', mrTitle: 'Optimize image loading', score: 82, date: '2024-01-15' },
      { reviewId: 'rev-202', mrTitle: 'Cache hot paths', score: 91, date: '2024-01-13' },
      { reviewId: 'rev-203', mrTitle: 'Reduce re-renders', score: 76, date: '2024-01-11' },
      { reviewId: 'rev-204', mrTitle: 'Database query tuning', score: 85, date: '2024-01-09' },
      { reviewId: 'rev-205', mrTitle: 'Bundle size reduction', score: 89, date: '2024-01-07' },
    ],
  },
  {
    id: 'expert-003',
    name: 'Quality Guardian',
    category: 'quality',
    icon: 'CircleCheck',
    enabled: true,
    weight: 90,
    description: 'Enforces code quality standards, consistency, and maintainability. Checks for code smells, duplication, and anti-patterns.',
    promptPreview: `You are a code quality expert. Review code for:
- Code duplication (DRY violations)
- Deep nesting and complex conditionals
- Magic numbers and string literals
- Inconsistent naming conventions
- Missing error handling and edge cases
- Excessive class/module coupling

Provide a quality score and prioritized refactoring suggestions.`,
    lastReviews: [
      { reviewId: 'rev-301', mrTitle: 'Refactor service layer', score: 87, date: '2024-01-14' },
      { reviewId: 'rev-302', mrTitle: 'Extract shared utilities', score: 93, date: '2024-01-12' },
      { reviewId: 'rev-303', mrTitle: 'Standardize error types', score: 80, date: '2024-01-10' },
    ],
  },
  {
    id: 'expert-004',
    name: 'Maintainability Sage',
    category: 'maintainability',
    icon: 'Tools',
    enabled: false,
    weight: 60,
    description: 'Evaluates long-term maintainability. Assesses modularity, documentation quality, and architectural consistency.',
    promptPreview: `You are a maintainability expert. Evaluate:
- Module boundaries and single responsibility
- Comment quality and self-documenting code
- Dependency direction and stability
- Configuration vs. hardcoding
- Testing coverage and testability
- API design and backward compatibility

Score each dimension and provide maintainability roadmap.`,
    lastReviews: [
      { reviewId: 'rev-401', mrTitle: 'Restructure API handlers', score: 72, date: '2024-01-13' },
      { reviewId: 'rev-402', mrTitle: 'Add module docs', score: 85, date: '2024-01-11' },
      { reviewId: 'rev-403', mrTitle: 'Refactor config loading', score: 68, date: '2024-01-09' },
    ],
  },
  {
    id: 'expert-005',
    name: 'Coverage Analyst',
    category: 'test-coverage',
    icon: 'CircleCheck',
    enabled: true,
    weight: 70,
    description: 'Analyzes test coverage gaps, missing edge cases, and ineffective tests. Suggests test strategies and mocking approaches.',
    promptPreview: `You are a test coverage expert. Analyze:
- Missing unit tests for critical paths
- Edge cases not covered (null, empty, boundary values)
- Fragile tests with hardcoded expectations
- Integration test gaps
- Mocking strategy effectiveness
- Mutation testing opportunities

Report coverage gaps with priority levels and test recommendations.`,
    lastReviews: [
      { reviewId: 'rev-501', mrTitle: 'Add unit tests for auth', score: 88, date: '2024-01-15' },
      { reviewId: 'rev-502', mrTitle: 'Fix flaky integration tests', score: 75, date: '2024-01-13' },
      { reviewId: 'rev-503', mrTitle: 'Cover edge cases', score: 92, date: '2024-01-11' },
      { reviewId: 'rev-504', mrTitle: 'Mock external services', score: 81, date: '2024-01-08' },
    ],
  },
  {
    id: 'expert-006',
    name: 'Doc Master',
    category: 'documentation',
    icon: 'Document',
    enabled: false,
    weight: 50,
    description: 'Reviews API documentation, README accuracy, inline comments, and changelog completeness. Ensures docs stay in sync with code.',
    promptPreview: `You are a documentation expert. Review for:
- API documentation completeness and accuracy
- README clarity and setup instructions
- Changelog entries for breaking changes
- Inline comments explaining complex logic
- Type definitions and JSDoc completeness
- Examples and usage guides

Flag outdated docs and suggest improvements.`,
    lastReviews: [
      { reviewId: 'rev-601', mrTitle: 'Update API docs', score: 70, date: '2024-01-14' },
      { reviewId: 'rev-602', mrTitle: 'Add migration guide', score: 83, date: '2024-01-12' },
      { reviewId: 'rev-603', mrTitle: 'Document new endpoints', score: 65, date: '2024-01-10' },
    ],
  },
  {
    id: 'expert-007',
    name: 'Dependency Scout',
    category: 'dependencies',
    icon: 'Link',
    enabled: true,
    weight: 65,
    description: 'Monitors dependency health, version conflicts, license compliance, and security advisories. Recommends upgrade paths.',
    promptPreview: `You are a dependency management expert. Check for:
- Outdated or vulnerable packages
- License compatibility issues
- Version conflicts and peer dependency mismatches
- Unused or redundant dependencies
- Supply chain security risks
- Ecosystem health and maintenance status

Provide upgrade priority matrix and risk assessment.`,
    lastReviews: [
      { reviewId: 'rev-701', mrTitle: 'Upgrade to React 18', score: 86, date: '2024-01-15' },
      { reviewId: 'rev-702', mrTitle: 'Fix peer dependency warnings', score: 79, date: '2024-01-13' },
      { reviewId: 'rev-703', mrTitle: 'Audit dependencies', score: 94, date: '2024-01-11' },
      { reviewId: 'rev-704', mrTitle: 'Remove unused packages', score: 90, date: '2024-01-09' },
    ],
  },
  {
    id: 'expert-008',
    name: 'Accessibility Ally',
    category: 'accessibility',
    icon: 'View',
    enabled: true,
    weight: 55,
    description: 'Reviews WCAG compliance, screen reader compatibility, keyboard navigation, and color contrast. Ensures inclusive design.',
    promptPreview: `You are an accessibility expert. Review for:
- WCAG 2.1 AA compliance (contrast, focus, labels)
- Screen reader compatibility (ARIA, alt text)
- Keyboard navigation and focus management
- Motion sensitivity and animation concerns
- Semantic HTML usage
- Form labeling and error communication

Report accessibility violations with severity levels.`,
    lastReviews: [
      { reviewId: 'rev-801', mrTitle: 'Fix focus indicators', score: 84, date: '2024-01-14' },
      { reviewId: 'rev-802', mrTitle: 'Add ARIA labels', score: 91, date: '2024-01-12' },
      { reviewId: 'rev-803', mrTitle: 'Improve color contrast', score: 77, date: '2024-01-10' },
    ],
  },
  {
    id: 'expert-009',
    name: 'Architecture Oracle',
    category: 'architecture',
    icon: 'OfficeBuilding',
    enabled: true,
    weight: 80,
    description: 'Evaluates architectural decisions, design patterns, scalability considerations, and tech stack alignment.',
    promptPreview: `You are a software architecture expert. Evaluate:
- Design pattern appropriateness and misuse
- Layer boundaries and dependency direction
- Scalability and performance implications
- Tech stack alignment with requirements
- Data flow and state management patterns
- API design and contract stability
- Microservices vs. monolith fit

Provide architectural review with trade-off analysis.`,
    lastReviews: [
      { reviewId: 'rev-901', mrTitle: 'Redesign caching layer', score: 89, date: '2024-01-15' },
      { reviewId: 'rev-902', mrTitle: 'Evaluate CQRS pattern', score: 82, date: '2024-01-13' },
      { reviewId: 'rev-903', mrTitle: 'Review database schema', score: 87, date: '2024-01-11' },
      { reviewId: 'rev-904', mrTitle: 'Assess event sourcing', score: 75, date: '2024-01-09' },
      { reviewId: 'rev-905', mrTitle: 'Validate service boundaries', score: 93, date: '2024-01-07' },
    ],
  },
]

// ========== Computed ==========
const categories = computed(() => [
  { value: 'all', label: 'All Categories' },
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
const fetchExperts = async () => {
  loading.value = true
  // Simulate API call
  await new Promise(resolve => setTimeout(resolve, 800))
  experts.value = mockExperts
  loading.value = false
}

const handleToggle = async (id: string, enabled: boolean) => {
  const expert = experts.value.find((e: Expert) => e.id === id)
  if (!expert) return

  // Optimistic update
  expert.enabled = enabled

  // Simulate API call
  await new Promise<void>(resolve => setTimeout(resolve, 300))

  ElNotification({
    title: enabled ? 'Expert Enabled' : 'Expert Disabled',
    message: `${expert.name} is now ${enabled ? 'enabled' : 'disabled'}`,
    type: enabled ? 'success' : 'warning',
    duration: 2000,
  })
}

const handleWeightChange = async (id: string, weight: number) => {
  const expert = experts.value.find((e: Expert) => e.id === id)
  if (!expert) return

  expert.weight = weight

  // Simulate debounced API call
  await new Promise<void>(resolve => setTimeout(resolve, 200))
}

const handleViewDetails = (expert: Expert) => {
  selectedExpert.value = expert
  detailModalVisible.value = true
}

const handleEditCard = (expert: Expert) => {
  editingExpert.value = { ...expert }
  editModalVisible.value = true
}

const saveEdit = async () => {
  if (!editingExpert.value) return

  const idx = experts.value.findIndex((e: Expert) => e.id === editingExpert.value!.id)
  if (idx === -1) return

  experts.value[idx] = { ...editingExpert.value }
  editModalVisible.value = false

  ElNotification({
    title: 'Changes Saved',
    message: `${editingExpert.value.name} has been updated`,
    type: 'success',
    duration: 2000,
  })

  editingExpert.value = null
}

const toggleGlobalEdit = () => {
  isEditing.value = !isEditing.value
  ElNotification({
    title: isEditing.value ? 'Edit Mode On' : 'Edit Mode Off',
    message: isEditing.value ? 'Weight sliders are now editable' : 'Changes have been saved',
    type: 'info',
    duration: 2000,
  })
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

// ========== Unsaved Changes Guard ==========
const hasUnsavedChanges = computed(() => isEditing.value)

const handleBeforeUnload = (e: BeforeUnloadEvent) => {
  if (hasUnsavedChanges.value) {
    e.preventDefault()
    e.returnValue = ''
  }
}

const unregisterGuard = router.beforeEach((to, from, next) => {
  if (hasUnsavedChanges.value && to.path !== from.path) {
    const confirm = window.confirm('You have unsaved changes. Leave without saving?')
    if (confirm) {
      isEditing.value = false
      next()
    } else {
      next(false)
    }
  } else {
    next()
  }
})

// ========== Lifecycle ==========
onMounted(() => {
  fetchExperts()
  window.addEventListener('beforeunload', handleBeforeUnload)
})

onBeforeUnmount(() => {
  window.removeEventListener('beforeunload', handleBeforeUnload)
  unregisterGuard()
})
</script>

<template>
  <div class="experts-page">
    <!-- Page Header -->
    <div class="page-header">
      <div class="header-title-section">
        <h1 class="page-title">Experts Management</h1>
        <p class="page-subtitle">Configure LLM review experts</p>
      </div>
      <div class="header-actions">
        <el-button
          :type="isEditing ? 'success' : 'default'"
:aria-label="isEditing ? 'Done editing' : 'Enter edit mode'"
          @click="toggleGlobalEdit"
        >
          <el-icon><component :is="isEditing ? Check : IconEdit" /></el-icon>
          {{ isEditing ? 'Done Editing' : 'Edit Mode' }}
        </el-button>
        <el-tooltip content="Coming soon" placement="top">
          <el-button type="primary" disabled :aria-label="'Add Expert (Coming soon)'">
            <el-icon><Plus /></el-icon>
            Add Expert
          </el-button>
        </el-tooltip>
      </div>
    </div>

    <!-- Stats Bar -->
    <div class="stats-bar">
      <el-card class="stat-card" shadow="never">
        <div class="stat-value">{{ enabledCount }}/{{ totalCount }}</div>
        <div class="stat-label">Active Experts</div>
      </el-card>
      <el-card class="stat-card" shadow="never">
        <div class="stat-value">{{ avgWeight }}%</div>
        <div class="stat-label">Avg Weight</div>
      </el-card>
      <el-card class="stat-card" shadow="never">
        <div class="stat-value">{{ totalCount }}</div>
        <div class="stat-label">Total Experts</div>
      </el-card>
    </div>

    <!-- Filters -->
    <div class="filters-bar">
      <el-input
        v-model="searchQuery"
        placeholder="Search experts..."
        clearable
        class="search-input"
      >
        <template #prefix>
          <el-icon><Search /></el-icon>
        </template>
      </el-input>
      <el-select v-model="filterCategory" placeholder="Category" class="category-select">
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
            <el-skeleton-item variant="circle" style="width: 40px; height: 40px; margin-bottom: 16px" />
            <el-skeleton-item variant="h3" style="width: 60%; margin-bottom: 12px" />
            <el-skeleton-item variant="text" style="width: 40%; margin-bottom: 16px" />
            <el-skeleton-item variant="p" style="width: 100%; margin-bottom: 8px" />
            <el-skeleton-item variant="p" style="width: 80%" />
          </div>
        </template>
      </el-skeleton>
    </div>

    <!-- Empty State -->
    <el-empty
      v-else-if="filteredExperts.length === 0"
      description="No experts found"
      :image-size="120"
    >
      <template #description>
        <p>No experts match your filters</p>
      </template>
      <el-button type="primary" @click="searchQuery = ''; filterCategory = 'all'">
        Clear Filters
      </el-button>
    </el-empty>

    <!-- Expert Grid -->
    <div v-else class="experts-grid">
      <ExpertCard
        v-for="(expert, index) in filteredExperts"
        :key="expert.id"
        :expert="expert"
        :index="index"
        :is-editing="isEditing"
        @toggle="handleToggle"
        @weight-change="handleWeightChange"
        @view-details="handleViewDetails"
        @edit-card="handleEditCard"
      />
    </div>

    <!-- Detail Modal -->
    <el-dialog
      v-model="detailModalVisible"
      title="Expert Details"
      width="600px"
      class="expert-dialog"
:aria-label="'Expert details dialog'"
      destroy-on-close
    >
      <div v-if="selectedExpert" class="detail-content">
        <div class="detail-header">
          <h2 class="detail-name">{{ selectedExpert.name }}</h2>
          <el-tag
            :color="categoryColorMap[selectedExpert.category]"
            effect="dark"
            size="small"
          >
            {{ categoryLabelMap[selectedExpert.category] }}
          </el-tag>
          <el-tag v-if="!selectedExpert.enabled" type="info" size="small" effect="plain">
            <el-icon><WarningFilled /></el-icon>
            Disabled
          </el-tag>
        </div>

        <el-divider />

        <div class="detail-section">
          <div class="detail-row">
            <span class="detail-label">Enabled:</span>
            <el-switch
              :aria-label="'Toggle ' + selectedExpert.name"
              :model-value="selectedExpert.enabled"
              @update:model-value="(val: boolean) => handleToggle(selectedExpert!.id, val)"
              :active-color="'var(--success)'"
              :inactive-color="'var(--offline)'"
            />
          </div>
          <div class="detail-row">
            <span class="detail-label">Weight:</span>
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
          <h4 class="section-title">Description</h4>
          <el-input
            type="textarea"
            :model-value="selectedExpert.description"
            readonly
            :rows="3"
            resize="none"
          />
        </div>

        <div class="detail-section">
          <h4 class="section-title">Prompt Preview</h4>
          <el-input
            type="textarea"
            :model-value="selectedExpert.promptPreview"
            readonly
            :rows="8"
            resize="none"
            class="prompt-textarea"
          />
        </div>

        <div class="detail-section">
          <h4 class="section-title">Last 5 Reviews</h4>
          <el-table
            :data="selectedExpert.lastReviews"
            size="small"
            class="reviews-table"
            @row-click="handleRowClick"
          >
            <el-table-column prop="mrTitle" label="MR Title" min-width="180" show-overflow-tooltip />
            <el-table-column prop="score" label="Score" width="90" align="center">
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
            <el-table-column prop="date" label="Date" width="100" align="right" />
          </el-table>
        </div>
      </div>
      <template #footer>
        <el-button @click="detailModalVisible = false">
          <el-icon><Close /></el-icon>
          Close
        </el-button>
      </template>
    </el-dialog>

    <!-- Edit Modal -->
    <el-dialog
      v-model="editModalVisible"
      title="Edit Expert"
      width="500px"
      class="expert-dialog"
:aria-label="'Expert details dialog'"
      destroy-on-close
    >
      <el-form v-if="editingExpert" label-position="top">
        <el-form-item label="Name">
          <el-input v-model="editingExpert.name" />
        </el-form-item>
        <el-form-item label="Category">
          <el-select v-model="editingExpert.category" style="width: 100%">
            <el-option
              v-for="(label, value) in categoryLabelMap"
              :key="value"
              :label="label"
              :value="value"
            />
          </el-select>
        </el-form-item>
        <el-form-item label="Enabled">
          <el-switch
            v-model="editingExpert.enabled"
            :active-color="'var(--success)'"
            :inactive-color="'var(--offline)'"
          />
        </el-form-item>
        <el-form-item label="Weight">
          <el-slider
            v-model="editingExpert.weight"
            :max="100"
            :step="5"
            :show-stops="true"
            show-input
          />
        </el-form-item>
        <el-form-item label="Description">
          <el-input
            v-model="editingExpert.description"
            type="textarea"
            :rows="4"
            resize="none"
          />
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button
          :aria-label="'Cancel editing'"
          @click="editModalVisible = false"
        >
          <el-icon><Close /></el-icon>
          Cancel
        </el-button>
        <el-button
          type="primary"
          :aria-label="'Save changes'"
          @click="saveEdit"
        >
          <el-icon><Check /></el-icon>
          Save Changes
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

/* Page Header */
.page-header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  margin-bottom: 24px;
  gap: 16px;
  flex-wrap: wrap;
}

.header-title-section {
  flex: 1;
  min-width: 0;
}

.page-title {
  font-size: 24px;
  font-weight: 600;
  color: var(--text-primary);
  margin: 0 0 4px 0;
}

.page-subtitle {
  font-size: 14px;
  color: var(--text-secondary);
  margin: 0;
}

.header-actions {
  display: flex;
  gap: 10px;
  flex-shrink: 0;
}

/* Stats Bar */
.stats-bar {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 16px;
  margin-bottom: 24px;
}

.stat-card {
  text-align: center;
  padding: 8px;
  background-color: var(--bg-card);
  border-color: var(--border-color);
}

.stat-value {
  font-size: 28px;
  font-weight: 700;
  color: var(--brand);
  font-family: var(--font-mono);
  line-height: 1.2;
  margin-bottom: 4px;
}

.stat-label {
  font-size: 13px;
  color: var(--text-secondary);
  font-weight: 500;
}

/* Filters */
.filters-bar {
  display: flex;
  gap: 12px;
  margin-bottom: 24px;
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
  gap: 16px;
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

/* Expert Grid */
.experts-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
  gap: 16px;
}

/* Detail Dialog */
.detail-content {
  padding: 0 4px;
}

.detail-header {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
  margin-bottom: 8px;
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
  gap: 16px;
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
  .page-header {
    flex-direction: column;
    align-items: stretch;
  }

  .header-actions {
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
