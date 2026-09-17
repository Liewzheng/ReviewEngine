<template>
  <el-dialog
    :model-value="open"
    :title="
      mode === 'add'
        ? $t('config.gitPlatforms.addDialogTitle')
        : $t('config.gitPlatforms.editDialogTitle')
    "
    width="var(--modal-w-lg)"
    append-to-body
    @close="emit('close')"
  >
    <!-- v-if remounts the form on every open, so the draft always starts
         from the row being edited (or blanks for add). -->
    <GitPlatformForm
      v-if="open"
      ref="formRef"
      :mode="mode"
      :platform="platform"
      :platforms="platforms"
      :editing-index="editingIndex"
      @save="emit('save', $event)"
      @cancel="emit('close')"
    />
    <template #footer>
      <el-button @click="emit('close')">{{ $t('common.cancel') }}</el-button>
      <el-button type="primary" @click="formRef?.save()">
        <el-icon v-if="mode === 'add'"><Plus /></el-icon>
        {{ mode === 'add' ? $t('config.gitPlatforms.addBtn') : $t('common.save') }}
      </el-button>
    </template>
  </el-dialog>
</template>

<script setup lang="ts">
import { ref } from 'vue';
import { Plus } from '@element-plus/icons-vue';
import type { GitPlatformConfig } from '../../types/config';
import GitPlatformForm from './GitPlatformForm.vue';

defineProps<{
  /** Dialog visibility (controlled by the parent). */
  open: boolean;
  /** 'add' for a brand-new entry, 'edit' for an existing row. */
  mode: 'add' | 'edit';
  /** The row being edited; undefined in add mode. */
  platform?: GitPlatformConfig;
  /** Every configured row — the unique-name check excludes the edited one. */
  platforms: GitPlatformConfig[];
  /** Index of the edited row; -1 in add mode. */
  editingIndex: number;
}>();

const emit = defineEmits<{
  /** The validated entry; the parent stages it for the auto-save. */
  save: [entry: GitPlatformConfig];
  close: [];
}>();

/** The form's expose: the Save button triggers the same validation +
 * submission the form's own submit path uses. */
const formRef = ref<{ save: () => Promise<void> }>();
</script>
