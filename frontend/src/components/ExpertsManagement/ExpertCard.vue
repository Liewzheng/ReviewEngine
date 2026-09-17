<script setup lang="ts">
import { computed } from 'vue'
import type { Component } from 'vue'
import {
  Lock,
  Lightning,
  CircleCheck,
  Tools,
  Document,
  Link,
  View,
  OfficeBuilding,
  Star,
  View as IconView,
  WarningFilled,
} from '@element-plus/icons-vue'
import type { Expert } from '../../types/expert'
import { categoryLabelMap } from '../../types/expert'

const props = defineProps<{
  expert: Expert
  index: number
  /* RENG-95: the effective report-level aggregation flag, so the aggregator
   * card can surface the "enabled but aggregation off" state on the row. */
  aggregated?: boolean
}>()

const emit = defineEmits<{
  (e: 'toggle', id: string, enabled: boolean): void
  (e: 'weight-change', id: string, weight: number): void
  (e: 'view-details', expert: Expert): void
}>()

/* RENG-95: the aggregator expert's slug id. The card carries the two-condition
 * rule's flag side: the aggregator runs only when the expert is enabled AND
 * the report-level aggregation flag is on. */
const isAggregator = computed(() => props.expert.id === 'aggregator')

const cardStyle = computed(() => ({
  opacity: props.expert.enabled ? 1 : 0.6,
  borderColor: props.expert.enabled ? 'var(--border-color)' : 'var(--offline)',
}))

const iconStyle = computed(() => ({
  color: props.expert.enabled ? 'var(--accent-primary)' : 'var(--text-secondary)',
}))

const categoryLabel = computed(() => categoryLabelMap[props.expert.category])

const iconComponents: Record<string, Component> = {
  Lock,
  Lightning,
  CircleCheck,
  Tools,
  Document,
  Link,
  View,
  OfficeBuilding,
  Star,
}

const currentIcon = computed(() => iconComponents[props.expert.icon] || Star)

const handleToggle = (val: boolean) => {
  emit('toggle', props.expert.id, val)
}

const handleWeightChange = (val: number) => {
  emit('weight-change', props.expert.id, val)
}

const handleViewDetails = () => {
  emit('view-details', props.expert)
}
</script>

<template>
  <div
    class="expert-card"
    :style="cardStyle"
    :class="{ 'is-enabled': expert.enabled, 'is-disabled': !expert.enabled }"
    :data-index="index"
  >
    <!-- Header -->
    <div class="card-header">
      <div class="header-left">
        <div class="expert-icon" :style="iconStyle">
          <el-icon :size="36"><component :is="currentIcon" /></el-icon>
        </div>
        <div class="expert-info">
          <h3 class="expert-name">{{ expert.name }}</h3>
          <div class="expert-tags">
            <el-tag size="small" effect="plain" class="category-tag">
              {{ categoryLabel }}
            </el-tag>
            <el-tag v-if="!expert.enabled" type="info" size="small" effect="plain" class="status-tag">
              <el-icon><WarningFilled /></el-icon>
              {{ $t('common.disabled') }}
            </el-tag>
            <!-- RENG-95: the two-condition rule on the aggregator's row — the
                 "12 enabled, 11 participated" surprise must be visible on the
                 page, not only inside a drawer. -->
            <el-tag
              v-if="isAggregator && expert.enabled && !aggregated"
              type="warning"
              size="small"
              effect="plain"
              class="aggregation-tag"
            >
              {{ $t('experts.aggregation.rowEnabledButOff') }}
            </el-tag>
            <el-tag
              v-if="isAggregator && expert.enabled && aggregated"
              type="success"
              size="small"
              effect="plain"
              class="aggregation-tag"
            >
              {{ $t('experts.aggregation.rowOn') }}
            </el-tag>
          </div>
        </div>
      </div>
      <div class="header-right">
        <el-tooltip :content="expert.enabled ? $t('common.enabled') : $t('common.disabled')" placement="top">
          <el-switch
            :aria-label="expert.enabled ? $t('common.enabled') : $t('common.disabled')"
            :model-value="expert.enabled"
            @update:model-value="handleToggle"
            :active-color="'var(--accent-success)'"
            :inactive-color="'var(--offline)'"
          />
        </el-tooltip>
      </div>
    </div>

    <!-- Weight Slider -->
    <div class="weight-section">
      <div class="weight-label">
        <span class="label-text">{{ $t('experts.card.weight') }}</span>
        <span class="weight-value">{{ expert.weight }}%</span>
      </div>
      <el-slider
        :model-value="expert.weight"
        @update:model-value="handleWeightChange"
        :max="100"
        :step="5"
        :show-stops="true"
        class="weight-slider"
      />
    </div>

    <!-- Description -->
    <div class="description-section">
      <el-tooltip
        :content="expert.description"
        placement="top-start"
        :show-after="500"
        :disabled="expert.description.length < 100"
      >
        <p class="expert-description">{{ expert.description }}</p>
      </el-tooltip>
    </div>

    <!-- Actions -->
    <div class="card-actions">
      <el-button size="small" :aria-label="$t('experts.viewDetailsAria', { name: expert.name })"
        @click="handleViewDetails">
        <el-icon><IconView /></el-icon>
        {{ $t('experts.card.viewDetails') }}
      </el-button>
    </div>
  </div>
</template>

<style scoped>
.expert-card {
  background: var(--bg-card);
  border: 1px solid var(--border-color);
  border-radius: var(--radius-md);
  padding: 20px;
  box-shadow: var(--shadow-card);
  transition: opacity 0.2s ease, filter 0.2s ease, border-color 0.2s ease, transform 0.2s ease;
  max-width: 360px;
  height: 100%;
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  animation: cardEnter 0.3s ease both;
  animation-delay: calc(v-bind('index') * 60ms);
}

.expert-card:hover {
  border-color: var(--brand);
  box-shadow: 0 0 0 1px var(--brand), var(--shadow-card);
}

@keyframes cardEnter {
  from {
    opacity: 0;
    transform: translateY(16px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

.card-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--space-3);
}

.header-left {
  display: flex;
  align-items: flex-start;
  gap: var(--space-3);
  flex: 1;
  min-width: 0;
}

.expert-icon {
  flex-shrink: 0;
  transition: color 0.2s ease;
  display: flex;
  align-items: center;
  justify-content: center;
  width: 40px;
  height: 40px;
}

.expert-info {
  flex: 1;
  min-width: 0;
}

.expert-name {
  font-size: 16px;
  font-weight: 600;
  color: var(--text-primary);
  margin: 0 0 6px 0;
  line-height: 1.3;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.expert-tags {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
}

/* R1.5: a muted surface, not one of the nine category colours — the icon
   already carries the category through its glyph. */
.category-tag {
  background: var(--bg-hover);
  color: var(--text-secondary);
  border: none;
  font-weight: 500;
}

.status-tag {
  display: flex;
  align-items: center;
  gap: var(--space-1);
  border-color: var(--offline);
  color: var(--offline);
}

/* RENG-95: the aggregator's two-condition hint. A warning fill carries the
   "enabled but aggregation off" surprise; a success fill the healthy state. */
.aggregation-tag {
  display: flex;
  align-items: center;
  gap: var(--space-1);
  font-weight: 500;
}

.header-right {
  flex-shrink: 0;
  padding-top: 2px;
}

.weight-section {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
}

.weight-label {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.label-text {
  font-size: 13px;
  color: var(--text-secondary);
  font-weight: 500;
}

.weight-value {
  font-size: 14px;
  font-weight: 600;
  color: var(--text-primary);
  font-family: var(--font-mono);
}

.weight-slider {
  width: 100%;
}

:deep(.weight-slider .el-slider__bar) {
  background-color: var(--accent-primary);
}

:deep(.weight-slider .el-slider__button) {
  border-color: var(--accent-primary);
  transition: all 0.1s ease;
}

.description-section {
  flex: 1;
  min-height: 0;
}

.expert-description {
  font-size: 13px;
  color: var(--text-secondary);
  line-height: 1.5;
  margin: 0;
  display: -webkit-box;
  -webkit-line-clamp: 3;
  -webkit-box-orient: vertical;
  overflow: hidden;
}

.card-actions {
  display: flex;
  gap: var(--space-2);
  margin-top: auto;
  padding-top: var(--space-2);
  border-top: 1px solid var(--border-color);
}

.card-actions .el-button {
  flex: 1;
  justify-content: center;
}

/* Flash border animation for updates */
.flash-border {
  animation: flashBorder 0.6s ease;
}

@keyframes flashBorder {
  0% {
    border-color: var(--accent-success);
    box-shadow: 0 0 0 2px var(--accent-success-ring);
  }
  100% {
    border-color: var(--border-color);
    box-shadow: var(--shadow-card);
  }
}
</style>
