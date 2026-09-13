<script setup lang="ts">
import { computed } from "vue";
import { i18n } from "../locales";
import type { Node } from "../types";
import { flattenVisible, pathLabel } from "../explorer/tree";

const { t } = i18n.global;

const props = defineProps<{
  nodes: Node[];
  openNodeId: string;
  expandedFolders: Set<string>;
  operationBusy: boolean;
  language: "zh" | "en";
}>();
const emit = defineEmits<{
  "toggle-folder": [id: string];
  "open-node": [id: string];
}>();

const rows = computed(() => flattenVisible(props.nodes, props.expandedFolders, props.language));

function rowTitle(id: string): string {
  return pathLabel(props.nodes, id);
}
function handleClick(node: Node) {
  if (node.kind === "folder") emit("toggle-folder", node.id);
  else emit("open-node", node.id);
}
</script>

<template>
  <aside class="explorer" role="tree" :aria-label="t('explorer.title')">
    <p v-if="!rows.length" class="explorer-empty">{{ t("explorer.empty") }}</p>
    <div
      v-for="row in rows"
      :key="row.node.id"
      class="tree-row"
      :style="{ paddingLeft: `${8 + row.depth * 12}px` }"
    >
      <button
        v-if="row.node.kind === 'folder'"
        type="button"
        class="tree-toggle"
        :aria-expanded="expandedFolders.has(row.node.id)"
        :aria-label="
          expandedFolders.has(row.node.id) ? t('explorer.collapseAria') : t('explorer.expandAria')
        "
        @click="emit('toggle-folder', row.node.id)"
      >
        {{ expandedFolders.has(row.node.id) ? "▾" : "▸" }}
      </button>
      <span v-else class="tree-toggle-spacer" aria-hidden="true"></span>
      <button
        type="button"
        class="tree-node"
        :class="[`tree-${row.node.kind}`, { active: row.node.id === openNodeId }]"
        :disabled="
          row.node.kind !== 'folder' && operationBusy && row.node.id !== openNodeId
        "
        :title="rowTitle(row.node.id)"
        @click="handleClick(row.node)"
      >
        {{ row.node.name }}
      </button>
    </div>
  </aside>
</template>

<style scoped>
.explorer {
  display: flex;
  flex-direction: column;
  overflow-y: auto;
  height: 100%;
}
.explorer-empty {
  padding: 12px;
  color: var(--fluent-text-secondary, #666);
  font-size: 13px;
}
.tree-row {
  display: flex;
  align-items: center;
  min-height: 24px;
}
.tree-toggle,
.tree-toggle-spacer {
  flex: none;
  width: 16px;
  height: 16px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
}
.tree-toggle {
  background: none;
  border: none;
  cursor: pointer;
  padding: 0;
  color: inherit;
}
.tree-node {
  flex: 1 1 auto;
  min-width: 0;
  text-align: left;
  background: none;
  border: none;
  padding: 2px 6px;
  cursor: pointer;
  color: inherit;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.tree-node:disabled {
  cursor: not-allowed;
  opacity: 0.6;
}
.tree-node.active {
  background: var(--fluent-accent-subtle, rgba(0, 120, 212, 0.12));
}
</style>
