<script setup lang="ts">
import { ref, computed } from 'vue'
import { useRouter } from 'vue-router'
import {
  ArrowRight,
  Connection,
} from '@element-plus/icons-vue'
import type { LlmProvider, LlmProviderStatus, TestResult } from '../../types/llm'

const props = defineProps<{
  provider: LlmProvider
  index: number
  testing?: boolean
}>()

const emit = defineEmits<{
  (e: 'test', provider: LlmProvider): void
}>()

const router = useRouter()

const testResult = ref<TestResult | null>(null)
const showAlert = ref(false)
const alertHovered = ref(false)

const statusConfig: Record<
  LlmProviderStatus,
  { label: string; type: 'success' | 'warning' | 'danger' | 'info' }
> = {
  healthy: { label: 'Healthy', type: 'success' },
  degraded: { label: 'Degraded', type: 'warning' },
  error: { label: 'Error', type: 'danger' },
  offline: { label: 'Offline', type: 'info' },
}

const statusInfo = computed(() => statusConfig[props.provider.status])

const latencyColor = computed(() => {
  const ms = props.provider.latencyMs
  if (ms < 500) return 'var(--success)'
  if (ms <= 1500) return 'var(--warning)'
  return 'var(--error)'
})

const errorRateColor = computed(() => {
  const rate = props.provider.errorRate
  if (rate < 0.01) return 'var(--success)'
  if (rate <= 0.05) return 'var(--warning)'
  return 'var(--error)'
})

const formattedErrorRate = computed(() => {
  return `${(props.provider.errorRate * 100).toFixed(1)}%`
})

const formattedRequests = computed(() => {
  return new Intl.NumberFormat('en-US').format(props.provider.requestCount)
})

const formattedLatency = computed(() => {
  if (!props.provider.configured || props.provider.status === 'offline') return '—'
  return `${props.provider.latencyMs} ms`
})

const formattedRequestsDisplay = computed(() => {
  if (!props.provider.configured || props.provider.status === 'offline') return '—'
  return formattedRequests.value
})

const formattedErrorRateDisplay = computed(() => {
  if (!props.provider.configured || props.provider.status === 'offline') return '—'
  return formattedErrorRate.value
})

const usagePercent = computed(() => {
  return props.provider.usagePercent ?? 0
})

const hasUsage = computed(() => props.provider.usagePercent !== undefined)

const hasSparkline = computed(() =>
  Array.isArray(props.provider.sparkline) && props.provider.sparkline.length > 1
)

const sparklinePath = computed(() => {
  if (!hasSparkline.value) return ''
  const data = props.provider.sparkline!
  const max = Math.max(...data, 1)
  const min = Math.min(...data)
  const range = max - min || 1
  const points = data.map((v, i) => {
    const x = (i / (data.length - 1)) * 100
    const y = 40 - ((v - min) / range) * 36
    return `${x},${y}`
  })
  return points.join(' ')
})

function handleTest() {
  if (!props.provider.configured) return
  showAlert.value = false
  testResult.value = null
  emit('test', props.provider)
}

function handleConfigure() {
  router.push({
    path: '/config',
    query: { tab: 'llm', provider: props.provider.id },
  })
}

function showTestResult(result: TestResult) {
  testResult.value = result
  showAlert.value = true
  if (!alertHovered.value) {
    setTimeout(() => {
      if (!alertHovered.value) {
        showAlert.value = false
      }
    }, 5000)
  }
}

function onAlertEnter() {
  alertHovered.value = true
}

function onAlertLeave() {
  alertHovered.value = false
  if (showAlert.value) {
    setTimeout(() => {
      if (!alertHovered.value) {
        showAlert.value = false
      }
    }, 5000)
  }
}

// Expose for parent
defineExpose({
  showTestResult,
  providerId: props.provider.id,
})
</script>

<template>
  <div
    class="provider-card"
    :class="[
      `status-${provider.status}`,
      { 'not-configured': !provider.configured },
    ]"
    :style="{ animationDelay: `${index * 50}ms` }"
  >
    <el-card shadow="hover" :body-style="{ padding: '20px' }">
      <!-- Header Row -->
      <div class="card-header">
        <div class="provider-info">
          <span class="provider-logo">{{ provider.logo }}</span>
          <span class="provider-name">{{ provider.name }}</span>
        </div>
        <el-tag
          :type="statusInfo.type"
          effect="dark"
          size="small"
          class="status-badge"
        >
          {{ statusInfo.label }}
        </el-tag>
      </div>

      <!-- Metrics Row -->
      <div class="metrics-row">
        <div class="metric">
          <div class="metric-label">Latency</div>
          <div
            class="metric-value"
            :style="{ color: formattedLatency !== '—' ? latencyColor : undefined }"
          >
            {{ formattedLatency }}
          </div>
        </div>
        <div class="metric">
          <div class="metric-label">Requests</div>
          <div class="metric-value">{{ formattedRequestsDisplay }}</div>
        </div>
        <div class="metric">
          <div class="metric-label">Errors</div>
          <div
            class="metric-value"
            :style="{
              color: formattedErrorRateDisplay !== '—' ? errorRateColor : undefined,
            }"
          >
            {{ formattedErrorRateDisplay }}
          </div>
        </div>
      </div>

      <!-- Usage Bar -->
      <div v-if="hasUsage && provider.configured" class="usage-bar">
        <el-progress
          :percentage="usagePercent"
          :stroke-width="6"
          :color="{
            custom: 'var(--brand)',
          }"
          :show-text="false"
        />
        <span class="usage-label">{{ usagePercent }}% capacity</span>
      </div>

      <!-- Sparkline -->
      <div v-if="hasSparkline && provider.configured" class="sparkline-wrap">
        <svg
          viewBox="0 0 100 40"
          preserveAspectRatio="none"
          class="sparkline-svg"
        >
          <polyline
            :points="sparklinePath"
            fill="none"
            stroke="var(--brand)"
            stroke-width="2"
            opacity="0.6"
          />
        </svg>
      </div>

      <!-- Test Result Alert -->
      <div
        v-if="showAlert && testResult"
        class="test-alert"
        @mouseenter="onAlertEnter"
        @mouseleave="onAlertLeave"
      >
        <el-alert
          :title="testResult.success ? 'Connected' : 'Connection failed'"
          :type="testResult.success ? 'success' : 'error'"
          :closable="true"
          @close="showAlert = false"
        >
          <template #default>
            <span v-if="testResult.success && testResult.latencyMs">
              Latency: {{ testResult.latencyMs }}ms
            </span>
            <span v-else-if="testResult.error">{{ testResult.error }}</span>
          </template>
        </el-alert>
      </div>

      <!-- Action Row -->
      <div class="action-row">
        <el-button
          size="small"
          :icon="Connection"
          :loading="testing"
          :disabled="!provider.configured"
          @click="handleTest"
        >
          Test Connection
        </el-button>
        <el-button
          size="small"
          :icon="ArrowRight"
          :type="!provider.configured ? 'primary' : 'default'"
          @click="handleConfigure"
        >
          Configure
        </el-button>
      </div>

      <!-- Last checked -->
      <div class="last-checked">
        Last checked: {{ new Date(provider.lastChecked).toLocaleString() }}
      </div>
    </el-card>
  </div>
</template>

<style scoped>
.provider-card {
  opacity: 0;
  animation: card-enter 0.25s ease forwards;
  min-width: 280px;
  max-width: 100%;
}

@keyframes card-enter {
  from {
    opacity: 0;
    transform: translateY(12px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

@keyframes flash-border {
  0% {
    box-shadow: 0 0 0 0 rgba(99, 102, 241, 0.6);
  }
  50% {
    box-shadow: 0 0 0 6px rgba(99, 102, 241, 0.2);
  }
  100% {
    box-shadow: 0 0 0 0 rgba(99, 102, 241, 0);
  }
}

.provider-card.status-change {
  animation: flash-border 0.6s ease;
}

.card-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 16px;
}

.provider-info {
  display: flex;
  align-items: center;
  gap: 10px;
}

.provider-logo {
  font-size: 28px;
  line-height: 1;
}

.provider-name {
  font-size: 16px;
  font-weight: 600;
  color: var(--text-primary);
}

.metrics-row {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 8px;
  margin-bottom: 16px;
}

.metric {
  text-align: center;
}

.metric-label {
  font-size: 11px;
  color: var(--text-secondary);
  text-transform: uppercase;
  letter-spacing: 0.05em;
  margin-bottom: 4px;
}

.metric-value {
  font-family: var(--font-mono);
  font-size: 18px;
  font-weight: 500;
  color: var(--text-primary);
  transition: color 0.2s ease;
}

.usage-bar {
  margin-bottom: 12px;
}

.usage-label {
  display: block;
  font-size: 12px;
  color: var(--text-secondary);
  margin-top: 4px;
}

.sparkline-wrap {
  margin-bottom: 12px;
  height: 40px;
  width: 100%;
}

.sparkline-svg {
  width: 100%;
  height: 40px;
  display: block;
}

.test-alert {
  margin-bottom: 12px;
  animation: alert-slide 0.2s ease;
}

@keyframes alert-slide {
  from {
    opacity: 0;
    transform: translateY(-6px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

.action-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  margin-top: 8px;
}

.last-checked {
  font-size: 11px;
  color: var(--text-secondary);
  margin-top: 12px;
  text-align: right;
}

.not-configured :deep(.el-card) {
  opacity: 0.85;
}
</style>
