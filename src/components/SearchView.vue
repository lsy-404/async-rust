<script setup lang="ts">
import { computed, nextTick, ref } from "vue";
import { FluentField } from "@platform-kit/fluent/vue";
import { i18n } from "../locales";
import type { Node, SearchField, SearchHit, SearchResults } from "../types";

const { t } = i18n.global;

const props = defineProps<{
  nodes: Node[];
  query: string;
  results: SearchResults | undefined;
  operationBusy: boolean;
}>();
const emit = defineEmits<{
  "update:query": [value: string];
  "open-node": [nodeId: string];
}>();

const fieldLabels: Record<SearchField, string> = {
  name: "search.field.name",
  transcription: "search.field.transcription",
  summary: "search.field.summary",
  content: "search.field.content",
  message: "search.field.message",
};
function fieldLabel(field: SearchField): string {
  return t(fieldLabels[field]);
}

interface Group {
  nodeId: string;
  node: Node | undefined;
  parentPath: string;
  hits: SearchHit[];
}
function ancestorPath(nodeId: string): string {
  const byId = new Map(props.nodes.map((item) => [item.id, item]));
  const names: string[] = [];
  let currentParent = byId.get(nodeId)?.parentId ?? null;
  while (currentParent) {
    const parent = byId.get(currentParent);
    if (!parent) break;
    names.unshift(parent.name);
    currentParent = parent.parentId;
  }
  return names.join(" / ");
}
const groups = computed<Group[]>(() => {
  const hits = props.results?.hits ?? [];
  const order: string[] = [];
  const byNode = new Map<string, SearchHit[]>();
  for (const hit of hits) {
    if (!byNode.has(hit.nodeId)) {
      byNode.set(hit.nodeId, []);
      order.push(hit.nodeId);
    }
    byNode.get(hit.nodeId)!.push(hit);
  }
  return order.map((nodeId) => ({
    nodeId,
    node: props.nodes.find((item) => item.id === nodeId),
    parentPath: ancestorPath(nodeId),
    hits: byNode.get(nodeId)!,
  }));
});
// Flat index across every hit row (grouping preserves this same order), used
// for ArrowUp/ArrowDown roving focus between result lines.
const flatHits = computed(() => props.results?.hits ?? []);

let fieldWrapEl: HTMLElement | null = null;
function setFieldWrap(el: unknown) {
  fieldWrapEl = (el as HTMLElement | null) ?? null;
}
function focusQuery() {
  const input = fieldWrapEl?.querySelector("input");
  input?.focus();
  input?.select();
}
defineExpose({ focusQuery });

const resultRefs = new Map<number, HTMLElement>();
function setResultRef(index: number, el: unknown) {
  if (el) resultRefs.set(index, el as HTMLElement);
  else resultRefs.delete(index);
}
async function focusResult(index: number) {
  await nextTick();
  resultRefs.get(index)?.focus();
}

function handleQueryInput(value: string) {
  emit("update:query", value);
}
function handleQueryKeydown(event: KeyboardEvent) {
  if (event.key === "Escape") {
    emit("update:query", "");
    return;
  }
  if (event.key === "ArrowDown" && flatHits.value.length) {
    event.preventDefault();
    void focusResult(0);
  }
}
function handleResultKeydown(event: KeyboardEvent, index: number) {
  if (event.key === "ArrowDown") {
    event.preventDefault();
    if (index + 1 < flatHits.value.length) void focusResult(index + 1);
    return;
  }
  if (event.key === "ArrowUp") {
    event.preventDefault();
    if (index > 0) void focusResult(index - 1);
    return;
  }
  if (event.key === "Enter") {
    event.preventDefault();
    activate(flatHits.value[index]!.nodeId);
  }
}
function activate(nodeId: string) {
  if (props.operationBusy) return;
  emit("open-node", nodeId);
}
</script>

<template>
  <aside class="search-view">
    <div :ref="setFieldWrap" class="search-field-wrap">
      <FluentField
        :label="t('search.title')"
        :model-value="query"
        :placeholder="t('search.placeholder')"
        @update:model-value="handleQueryInput"
        @keydown="handleQueryKeydown"
      />
    </div>
    <p v-if="!query.trim()" class="search-empty">{{ t("search.emptyPrompt") }}</p>
    <p v-else-if="!groups.length" class="search-empty">
      {{ t("search.noResults", { q: query }) }}
    </p>
    <div v-else class="search-results">
      <section v-for="group in groups" :key="group.nodeId" class="search-group">
        <header class="search-group-header">
          <span class="search-kind-glyph" aria-hidden="true">{{
            group.node?.kind === "folder" ? "▸" : group.node?.kind === "material" ? "≡" : "♪"
          }}</span>
          <span class="search-group-name">{{ group.node?.name ?? group.nodeId }}</span>
          <span v-if="group.parentPath" class="search-group-path">{{ group.parentPath }}</span>
        </header>
        <button
          v-for="hit in group.hits"
          :key="`${hit.nodeId}-${hit.field}-${hit.piece.matched}-${hit.piece.before.length}`"
          type="button"
          class="search-hit"
          :class="{ busy: operationBusy }"
          :ref="(el) => setResultRef(flatHits.indexOf(hit), el)"
          :disabled="operationBusy"
          @click="activate(hit.nodeId)"
          @keydown="handleResultKeydown($event, flatHits.indexOf(hit))"
        >
          <span class="search-field-label">{{ fieldLabel(hit.field) }}</span>
          <span class="search-piece"
            >{{ hit.piece.before }}<mark>{{ hit.piece.matched }}</mark>{{ hit.piece.after }}</span
          >
        </button>
      </section>
      <p v-if="results?.truncated" class="search-truncated">{{ t("search.truncated") }}</p>
    </div>
  </aside>
</template>

<style scoped>
.search-view {
  width: 264px;
  flex: none;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  border-right: 1px solid var(--fluent-border);
  background: color-mix(in srgb, var(--fluent-surface) 78%, transparent);
}
.search-field-wrap {
  padding: 8px 8px 4px;
}
.search-empty {
  padding: 12px;
  color: var(--fluent-text-secondary, #666);
  font-size: 13px;
}
.search-results {
  overflow-y: auto;
  flex: 1 1 auto;
}
.search-group {
  padding: 4px 0;
  border-bottom: 1px solid var(--fluent-border);
}
.search-group-header {
  display: flex;
  align-items: baseline;
  gap: 6px;
  padding: 4px 8px;
  font-weight: 600;
}
.search-kind-glyph {
  flex: none;
  opacity: 0.7;
}
.search-group-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.search-group-path {
  flex: 1 1 auto;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--fluent-text-secondary, #666);
  font-weight: 400;
  font-size: 12px;
}
.search-hit {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 2px;
  width: 100%;
  padding: 4px 8px 4px 24px;
  border: none;
  background: none;
  color: inherit;
  text-align: left;
  cursor: pointer;
  font: inherit;
}
.search-hit:hover {
  background: color-mix(in srgb, var(--fluent-muted) 10%, transparent);
}
.search-hit:disabled,
.search-hit.busy {
  cursor: not-allowed;
  opacity: 0.6;
}
.search-field-label {
  font-size: 11px;
  color: var(--fluent-text-secondary, #666);
}
.search-piece {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  max-width: 100%;
}
.search-piece mark {
  background: color-mix(in srgb, var(--fluent-accent) 35%, transparent);
  color: inherit;
}
.search-truncated {
  padding: 8px;
  font-size: 12px;
  color: var(--fluent-text-secondary, #666);
}
</style>
