<script setup lang="ts">
import { computed } from 'vue'
import { marked } from 'marked'
import DOMPurify from 'dompurify'

interface Props {
  /** Markdown source to render as HTML. Empty/omitted renders nothing. */
  source?: string
}

const props = withDefaults(defineProps<Props>(), {
  source: '',
})

/**
 * Render order matters: `marked.parse` first, `DOMPurify.sanitize` second.
 * Sanitizing the *final* HTML strips raw tags smuggled through the markdown
 * source (script, event handlers, javascript: links) before it reaches
 * v-html. `gfm: true` enables tables/strikethrough/autolinks; `breaks: false`
 * keeps paragraph structure driven by blank lines instead of every newline.
 */
const html = computed(() =>
  DOMPurify.sanitize(marked.parse(props.source, { async: false, gfm: true, breaks: false })),
)
</script>

<template>
  <!-- source is sanitized in the computed above before v-html receives it -->
  <div class="markdown-view" v-html="html" />
</template>

<style scoped>
.markdown-view {
  font-size: 13px;
  line-height: 1.6;
  color: var(--text-primary);
  word-break: break-word;
}

.markdown-view :deep(p) {
  margin: 0 0 8px;
}

.markdown-view :deep(h1),
.markdown-view :deep(h2),
.markdown-view :deep(h3),
.markdown-view :deep(h4),
.markdown-view :deep(h5),
.markdown-view :deep(h6) {
  margin: 14px 0 6px;
  font-weight: 600;
  line-height: 1.35;
  color: var(--text-primary);
}

.markdown-view :deep(h1) {
  font-size: 16px;
}

.markdown-view :deep(h2) {
  font-size: 15px;
}

.markdown-view :deep(h3) {
  font-size: 14px;
}

.markdown-view :deep(h4),
.markdown-view :deep(h5),
.markdown-view :deep(h6) {
  font-size: 13px;
}

.markdown-view :deep(ul),
.markdown-view :deep(ol) {
  margin: 4px 0 8px;
  padding-left: 20px;
}

.markdown-view :deep(li) {
  margin: 2px 0;
}

.markdown-view :deep(code) {
  padding: 1px 4px;
  background: var(--bg-card);
  border-radius: 4px;
  font-family: var(--font-mono);
  font-size: 12px;
}

.markdown-view :deep(pre) {
  margin: 6px 0 10px;
  padding: 8px 12px;
  background: var(--bg-card);
  border: 1px solid var(--border-color);
  border-radius: var(--radius-sm);
  overflow-x: auto;
  font-family: var(--font-mono);
  font-size: 12px;
  line-height: 1.5;
}

.markdown-view :deep(pre code) {
  padding: 0;
  background: transparent;
  border-radius: 0;
  font-size: inherit;
}

.markdown-view :deep(blockquote) {
  margin: 6px 0 10px;
  padding: 2px 12px;
  border-left: 3px solid var(--border-color);
  border-radius: 0 var(--radius-sm) var(--radius-sm) 0;
  background: var(--bg-hover);
  color: var(--text-secondary);
}

.markdown-view :deep(blockquote p:last-child) {
  margin-bottom: 0;
}

.markdown-view :deep(table) {
  margin: 6px 0 10px;
  border-collapse: collapse;
  font-size: 12px;
}

.markdown-view :deep(th),
.markdown-view :deep(td) {
  padding: 4px 8px;
  border: 1px solid var(--border-color);
  text-align: left;
}

.markdown-view :deep(th) {
  background: var(--bg-card);
  font-weight: 600;
}

.markdown-view :deep(hr) {
  margin: 12px 0;
  border: none;
  border-top: 1px solid var(--border-color);
}

/* Avoid gaps at the very top/bottom of the surrounding panel. */
.markdown-view :deep(> :first-child) {
  margin-top: 0;
}

.markdown-view :deep(> :last-child) {
  margin-bottom: 0;
}
</style>
