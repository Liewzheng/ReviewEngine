<script setup lang="ts">
import { ref, computed, watch, onMounted } from 'vue'
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
  loading?: boolean
}>()

const emit = defineEmits<{
  (e: 'test', provider: LlmProvider): void
}>()

const router = useRouter()

const testResult = ref<TestResult | null>(null)
const showAlert = ref(false)
const alertHovered = ref(false)

// Animation states
const statusChanging = ref(false)
const sparklineAnimated = ref(false)

// Latency smooth transition
const displayLatency = ref(props.provider.latencyMs)
const animatingLatency = ref(false)

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

// Watch status changes for flash-border animation
watch(() => props.provider.status, (newVal, oldVal) => {
  if (newVal !== oldVal && oldVal !== undefined) {
    statusChanging.value = true
    setTimeout(() => {
      statusChanging.value = false
    }, 600)
  }
})

// Watch latency changes with smooth number transition (0.3s)
watch(() => props.provider.latencyMs, (newVal, oldVal) => {
  if (newVal === oldVal || animatingLatency.value) return
  const start = oldVal ?? 0
  const end = newVal
  const duration = 300
  const startTime = performance.now()

  animatingLatency.value = true
  function animate(currentTime: number) {
    const elapsed = currentTime - startTime
    const progress = Math.min(elapsed / duration, 1)
    displayLatency.value = Math.round(start + (end - start) * progress)
    if (progress < 1) {
      requestAnimationFrame(animate)
    } else {
      animatingLatency.value = false
    }
  }
  requestAnimationFrame(animate)
})

const latencyColor = computed(() => {
  const ms = displayLatency.value
  if (ms === 0 || !props.provider.configured || props.provider.status === 'offline') return ''
  if (ms < 500) return 'var(--success)'
  if (ms <= 1500) return 'var(--warning)'
  return 'var(--error)'
})

const latencyStyle = computed(() => {
  if (formattedLatency.value === '—') return {}
  return { color: latencyColor.value }
})

const errorRateColor = computed(() => {
  // When status is error, force red regardless of error rate value
  if (props.provider.status === 'error') return 'var(--error)'
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
  return `${displayLatency.value} ms`
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

// Sparkline mount animation
onMounted(() => {
  if (hasSparkline.value) {
    // Trigger animation after a short delay to ensure DOM is ready
    setTimeout(() => {
      sparklineAnimated.value = true
    }, 100)
  }
})

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
      { 'not-configured': !provider.configured, 'status-change': statusChanging },
    ]"
    :style="{ animationDelay: `${index * 50}ms` }"
  >
    <el-card
      v-loading="loading"
      shadow="hover"
      :body-style="{ padding: '20px' }"
      class="provider-card-inner"
    >
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
          :class="{ 'offline-badge': provider.status === 'offline' }"
        >
          {{ statusInfo.label }}
        </el-tag>
      </div>

      <!-- Metrics Row -->
      <div class="metrics-row">
        <div class="metric">
          <div class="metric-label">Latency</div>
          <div class="metric-value" :style="latencyStyle">
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
          :color="'var(--brand)'"
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
            class="sparkline-line"
            :class="{ animated: sparklineAnimated }"
          />
        </svg>
      </div>

      <!-- Test Result Alert with fade-out transition -->
      <Transition name="alert-fade">
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
      </Transition>

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
  min-width: 320px;
  max-width: 400px;
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

.provider-card.status-change .provider-card-inner :deep(.el-card) {
  animation: flash-border 0.6s ease;
}

.provider-card-inner :deep(.el-card) {
  transition: border-color 0.2s ease, box-shadow 0.2s ease, transform 0.2s ease;
}

.provider-card-inner:hover :deep(.el-card) {
  border-color: var(--brand);
  box-shadow: 0 0 0 1px var(--brand), var(--shadow-card);
  transform: translateY(-2px);
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
  font-size: 32px;
  line-height: 1;
}

.provider-name {
  font-size: 16px;
  font-weight: 600;
  color: var(--text-primary);
}

.status-badge.offline-badge {
  background-color: var(--offline) !important;
  border-color: var(--offline) !important;
  color: #fff !important;
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

.sparkline-line {
  stroke-dasharray: 1000;
  stroke-dashoffset: 1000;
}

.sparkline-line.animated {
  animation: sparkline-draw 1s ease forwards;
}

@keyframes sparkline-draw {
  to {
    stroke-dashoffset: 0;
  }
}

.test-alert {
  margin-bottom: 12px;
}

.alert-fade-enter-active {
  animation: alert-slide-in 0.2s ease;
}

.alert-fade-leave-active {
  animation: alert-fade-out 0.3s ease forwards;
}

@keyframes alert-slide-in {
  from {
    opacity: 0;
    transform: translateY(-6px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

@keyframes alert-fade-out {
  from {
    opacity: 1;
    transform: translateY(0);
  }
  to {
    opacity: 0;
    transform: translateY(-4px);
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
