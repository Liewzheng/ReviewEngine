<script setup lang="ts">
import { computed } from 'vue'
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
  Edit,
  WarningFilled,
} from '@element-plus/icons-vue'
import type { Expert } from '../../types/expert'
import { categoryColorMap, categoryLabelMap } from '../../types/expert'

const props = defineProps<{
  expert: Expert
  index: number
  isEditing?: boolean
}>()

const emit = defineEmits<{
  (e: 'toggle', id: string, enabled: boolean): void
  (e: 'weight-change', id: string, weight: number): void
  (e: 'view-details', expert: Expert): void
  (e: 'edit-card', expert: Expert): void
}>()

const cardStyle = computed(() => ({
  opacity: props.expert.enabled ? 1 : 0.6,
  borderColor: props.expert.enabled ? 'var(--border-color)' : 'var(--offline)',
}))

const iconStyle = computed(() => ({
  filter: props.expert.enabled ? 'none' : 'grayscale(100%)',
  color: categoryColorMap[props.expert.category],
}))

const categoryColor = computed(() => categoryColorMap[props.expert.category])
const categoryLabel = computed(() => categoryLabelMap[props.expert.category])

const iconComponents: Record<string, any> = {
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

const handleEdit = () => {
  emit('edit-card', props.expert)
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
            <el-tag :color="categoryColor" effect="dark" size="small" class="category-tag">
              {{ categoryLabel }}
            </el-tag>
            <el-tag v-if="!expert.enabled" type="info" size="small" effect="plain" class="status-tag">
              <el-icon><WarningFilled /></el-icon>
              Disabled
            </el-tag>
          </div>
        </div>
      </div>
      <div class="header-right">
        <el-tooltip :content="expert.enabled ? 'Enabled' : 'Disabled'" placement="top">
          <el-switch
            :model-value="expert.enabled"
            @update:model-value="handleToggle"
            :active-color="'var(--success)'"
            :inactive-color="'var(--offline)'"
          />
        </el-tooltip>
      </div>
    </div>

    <!-- Weight Slider -->
    <div class="weight-section">
      <div class="weight-label">
        <span class="label-text">Weight</span>
        <span class="weight-value">{{ expert.weight }}%</span>
      </div>
      <el-slider
        :model-value="expert.weight"
        @update:model-value="handleWeightChange"
        :max="100"
        :step="5"
        :show-stops="true"
        :disabled="!isEditing"
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
      <el-button size="small" @click="handleViewDetails">
        <el-icon><IconView /></el-icon>
        View Details
      </el-button>
      <el-button size="small" type="primary" @click="handleEdit">
        <el-icon><Edit /></el-icon>
        Edit
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
  display: flex;
  flex-direction: column;
  gap: 16px;
  animation: cardEnter 0.3s ease both;
  animation-delay: calc(v-bind('index') * 60ms);
}

.expert-card:hover {
  transform: translateY(-2px);
  box-shadow: 0 8px 12px -2px rgba(0, 0, 0, 0.4), 0 4px 6px -1px rgba(0, 0, 0, 0.3);
}

[data-theme="light"] .expert-card:hover {
  box-shadow: 0 8px 12px -2px rgba(0, 0, 0, 0.1), 0 4px 6px -1px rgba(0, 0, 0, 0.06);
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
  gap: 12px;
}

.header-left {
  display: flex;
  align-items: flex-start;
  gap: 12px;
  flex: 1;
  min-width: 0;
}

.expert-icon {
  flex-shrink: 0;
  transition: filter 0.2s ease;
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

.category-tag {
  border: none;
  font-weight: 500;
}

.status-tag {
  display: flex;
  align-items: center;
  gap: 4px;
  border-color: var(--offline);
  color: var(--offline);
}

.header-right {
  flex-shrink: 0;
  padding-top: 2px;
}

.weight-section {
  display: flex;
  flex-direction: column;
  gap: 8px;
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
  background-color: var(--brand);
}

:deep(.weight-slider .el-slider__button) {
  border-color: var(--brand);
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
  gap: 8px;
  margin-top: auto;
  padding-top: 8px;
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
    border-color: var(--success);
    box-shadow: 0 0 0 2px rgba(34, 197, 94, 0.3);
  }
  100% {
    border-color: var(--border-color);
    box-shadow: var(--shadow-card);
  }
}
</style>
