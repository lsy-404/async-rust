<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, watch } from "vue";
import { FluentButton, FluentDialog } from "@platform-kit/fluent/vue";
import { i18n } from "../locales";
import type { Node } from "../types";
import { flattenVisible, pathLabel, subtreeCounts } from "../explorer/tree";

const { t } = i18n.global;

const props = defineProps<{
  nodes: Node[];
  openNodeId: string;
  expandedFolders: Set<string>;
  operationBusy: boolean;
  language: "zh" | "en";
  focusedNodeId: string | null;
  contextMenu: { x: number; y: number; nodeId: string | null } | undefined;
  inlineCreate: { parentId: string | null; kind: "session" | "folder" } | undefined;
  createDraft: string;
  renamingId: string;
  renameDraft: string;
  deleteTarget: { nodeId: string } | undefined;
}>();
const emit = defineEmits<{
  "toggle-folder": [id: string];
  "open-node": [id: string];
  "focus-node": [id: string | null];
  "context-menu": [payload: { x: number; y: number; nodeId: string | null }];
  "close-context-menu": [];
  "start-create": [kind: "session" | "folder"];
  "cancel-create": [];
  "commit-create": [];
  "update:createDraft": [value: string];
  "start-import": [];
  "start-rename": [id: string];
  "cancel-rename": [];
  "commit-rename": [];
  "update:renameDraft": [value: string];
  "open-delete-dialog": [id: string];
  "cancel-delete": [];
  "confirm-delete": [];
}>();

type DisplayRow =
  | { type: "node"; node: Node; depth: number }
  | { type: "create"; parentId: string | null; depth: number };

const flatRows = computed(() =>
  flattenVisible(props.nodes, props.expandedFolders, props.language),
);

const rows = computed<DisplayRow[]>(() => {
  const nodeRows: DisplayRow[] = flatRows.value.map((row) => ({
    type: "node",
    node: row.node,
    depth: row.depth,
  }));
  const create = props.inlineCreate;
  if (!create) return nodeRows;
  if (create.parentId === null) {
    return [{ type: "create", parentId: null, depth: 0 }, ...nodeRows];
  }
  const parentIndex = nodeRows.findIndex(
    (row) => row.type === "node" && row.node.id === create.parentId,
  );
  if (parentIndex === -1) return nodeRows;
  const depth = (nodeRows[parentIndex] as { depth: number }).depth + 1;
  const result = [...nodeRows];
  result.splice(parentIndex + 1, 0, { type: "create", parentId: create.parentId, depth });
  return result;
});

function rowTitle(id: string): string {
  return pathLabel(props.nodes, id);
}

// Function refs (not string refs, which Vue collects into arrays inside a
// v-for) so the activity bar's Explorer shortcut can focus a specific row.
const rowEls = new Map<string, HTMLElement>();
function setRowRef(id: string, el: unknown) {
  if (el) rowEls.set(id, el as HTMLElement);
  else rowEls.delete(id);
}
// Focuses the currently open or focused row, else the first row - used by
// the activity bar's Cmd/Ctrl+Shift+E shortcut.
function focusRow() {
  const targetId = props.focusedNodeId ?? (props.openNodeId || undefined) ?? flatRows.value[0]?.node.id;
  if (!targetId) return;
  rowEls.get(targetId)?.focus();
}
defineExpose({ focusRow });
function handleClick(node: Node) {
  emit("focus-node", node.id);
  if (node.kind === "folder") emit("toggle-folder", node.id);
  else emit("open-node", node.id);
}
function handleContextMenu(event: MouseEvent, nodeId: string | null) {
  emit("focus-node", nodeId);
  emit("context-menu", { x: event.clientX, y: event.clientY, nodeId });
}
function handleBlankClick() {
  emit("focus-node", null);
}
function handleF2(node: Node) {
  emit("start-rename", node.id);
}

const contextNode = computed(() =>
  props.contextMenu?.nodeId
    ? props.nodes.find((item) => item.id === props.contextMenu!.nodeId)
    : undefined,
);
function closeMenu() {
  emit("close-context-menu");
}
function menuNewSession() {
  emit("start-create", "session");
}
function menuNewFolder() {
  emit("start-create", "folder");
}
function menuImport() {
  emit("start-import");
}
function menuOpen() {
  if (contextNode.value) emit("open-node", contextNode.value.id);
  closeMenu();
}
function menuRename() {
  if (contextNode.value) emit("start-rename", contextNode.value.id);
}
function menuDelete() {
  if (contextNode.value) emit("open-delete-dialog", contextNode.value.id);
}
function handleOutsideClick(event: MouseEvent) {
  if (!(event.target as HTMLElement).closest(".context-menu")) closeMenu();
}
function handleGlobalKeydown(event: KeyboardEvent) {
  if (event.key === "Escape") closeMenu();
}
watch(
  () => Boolean(props.contextMenu),
  (open) => {
    if (open) {
      document.addEventListener("mousedown", handleOutsideClick);
      document.addEventListener("keydown", handleGlobalKeydown);
    } else {
      document.removeEventListener("mousedown", handleOutsideClick);
      document.removeEventListener("keydown", handleGlobalKeydown);
    }
  },
);
onBeforeUnmount(() => {
  document.removeEventListener("mousedown", handleOutsideClick);
  document.removeEventListener("keydown", handleGlobalKeydown);
});

// A function ref (rather than a string ref) so Vue assigns the element
// directly even though this row renders inside a v-for.
let createInputEl: HTMLInputElement | null = null;
function setCreateInputRef(el: unknown) {
  createInputEl = (el as HTMLInputElement | null) ?? null;
}
watch(
  () => Boolean(props.inlineCreate),
  async (active) => {
    if (!active) return;
    await nextTick();
    createInputEl?.focus();
  },
);
function commitCreate() {
  emit("commit-create");
}
function cancelCreate() {
  emit("cancel-create");
}
function handleCreateInput(event: Event) {
  emit("update:createDraft", (event.target as HTMLInputElement).value);
}

let renameInputEl: HTMLInputElement | null = null;
function setRenameInputRef(el: unknown) {
  renameInputEl = (el as HTMLInputElement | null) ?? null;
}
watch(
  () => props.renamingId,
  async (id) => {
    if (!id) return;
    await nextTick();
    const el = renameInputEl;
    if (!el) return;
    el.focus();
    const node = props.nodes.find((item) => item.id === id);
    const lastDot = node?.kind === "material" ? node.name.lastIndexOf(".") : -1;
    if (lastDot > 0) el.setSelectionRange(0, lastDot);
    else el.select();
  },
);
function commitRename() {
  emit("commit-rename");
}
function cancelRename() {
  emit("cancel-rename");
}
function handleRenameInput(event: Event) {
  emit("update:renameDraft", (event.target as HTMLInputElement).value);
}

// Joins with the locale's separator, using "and"/"和" only before the last
// part; e.g. "2 subfolders, 1 session and 3 materials".
function joinList(parts: string[]): string {
  if (parts.length <= 1) return parts[0] ?? "";
  const and = t("explorer.delete.listAnd");
  const sep = t("explorer.delete.listSeparator");
  return parts.slice(0, -1).join(sep) + and + parts[parts.length - 1];
}
const deleteNode = computed(() =>
  props.deleteTarget
    ? props.nodes.find((item) => item.id === props.deleteTarget!.nodeId)
    : undefined,
);
const deleteMessage = computed(() => {
  const node = deleteNode.value;
  if (!node) return "";
  if (node.kind === "session") return t("explorer.delete.session", { name: node.name });
  if (node.kind === "material") return t("explorer.delete.material", { name: node.name });
  const counts = subtreeCounts(props.nodes, node.id);
  const parts: string[] = [];
  if (counts.folders) parts.push(t("explorer.delete.countFolders", { n: counts.folders }));
  if (counts.sessions) parts.push(t("explorer.delete.countSessions", { n: counts.sessions }));
  if (counts.materials) parts.push(t("explorer.delete.countMaterials", { n: counts.materials }));
  if (!parts.length) return t("explorer.delete.folderEmpty", { name: node.name });
  return t("explorer.delete.folderWithContents", { name: node.name, list: joinList(parts) });
});
</script>

<template>
  <aside
    class="explorer"
    role="tree"
    :aria-label="t('explorer.title')"
    @contextmenu.prevent="handleContextMenu($event, null)"
  >
    <p v-if="!rows.length && !inlineCreate" class="explorer-empty">
      {{ t("explorer.empty") }}
    </p>
    <template v-for="row in rows" :key="row.type === 'node' ? row.node.id : `create-${row.parentId}`">
      <div
        v-if="row.type === 'create'"
        class="tree-row tree-row-create"
        :style="{ paddingLeft: `${8 + row.depth * 12}px` }"
      >
        <span class="tree-toggle-spacer" aria-hidden="true"></span>
        <input
          :ref="setCreateInputRef"
          class="inline-input"
          :value="createDraft"
          @input="handleCreateInput"
          @keydown.enter="commitCreate"
          @keydown.escape="cancelCreate"
          @blur="commitCreate"
        />
      </div>
      <div
        v-else
        class="tree-row"
        :class="{
          'context-target': contextMenu?.nodeId === row.node.id,
          focused: focusedNodeId === row.node.id,
        }"
        :style="{ paddingLeft: `${8 + row.depth * 12}px` }"
        @contextmenu.prevent.stop="handleContextMenu($event, row.node.id)"
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
        <input
          v-if="renamingId === row.node.id"
          :ref="setRenameInputRef"
          class="inline-input"
          :value="renameDraft"
          @input="handleRenameInput"
          @keydown.enter="commitRename"
          @keydown.escape="cancelRename"
          @blur="commitRename"
        />
        <button
          v-else
          :ref="(el) => setRowRef(row.node.id, el)"
          type="button"
          class="tree-node"
          :class="[`tree-${row.node.kind}`, { active: row.node.id === openNodeId }]"
          :disabled="
            row.node.kind !== 'folder' && operationBusy && row.node.id !== openNodeId
          "
          :title="rowTitle(row.node.id)"
          @click="handleClick(row.node)"
          @keydown.f2.prevent="handleF2(row.node)"
        >
          {{ row.node.name }}
        </button>
      </div>
    </template>
    <div class="explorer-blank" @click="handleBlankClick"></div>
    <div
      v-if="contextMenu"
      class="context-menu"
      role="menu"
      :style="{ left: `${contextMenu.x}px`, top: `${contextMenu.y}px` }"
    >
      <button
        v-if="!contextNode || contextNode.kind === 'folder'"
        type="button"
        role="menuitem"
        @click="menuNewSession"
      >
        {{ t("explorer.newSession") }}
      </button>
      <button
        v-if="!contextNode || contextNode.kind === 'folder'"
        type="button"
        role="menuitem"
        @click="menuNewFolder"
      >
        {{ t("explorer.newFolder") }}
      </button>
      <button
        v-if="!contextNode || contextNode.kind === 'folder'"
        type="button"
        role="menuitem"
        @click="menuImport"
      >
        {{ t("explorer.importMaterial") }}
      </button>
      <template v-if="contextNode">
        <div class="context-menu-separator"></div>
        <button
          v-if="contextNode.kind !== 'folder'"
          type="button"
          role="menuitem"
          @click="menuOpen"
        >
          {{ t("explorer.open") }}
        </button>
        <button type="button" role="menuitem" @click="menuRename">
          {{ t("explorer.rename") }}
        </button>
        <button type="button" role="menuitem" class="danger" @click="menuDelete">
          {{ t("explorer.deleteAction") }}
        </button>
      </template>
    </div>
  </aside>
  <FluentDialog v-if="deleteTarget" :open="true" :label="t('explorer.delete.dialogTitle')">
    <template #title>
      <h2>{{ t("explorer.delete.dialogTitle") }}</h2>
    </template>
    <template #default>
      <p>{{ deleteMessage }}</p>
    </template>
    <template #footer>
      <FluentButton tone="subtle" @click="emit('cancel-delete')">{{
        t("common.cancel")
      }}</FluentButton>
      <FluentButton tone="danger" :disabled="operationBusy" @click="emit('confirm-delete')">{{
        t("common.delete")
      }}</FluentButton>
    </template>
  </FluentDialog>
</template>

<style scoped>
.explorer {
  display: flex;
  flex-direction: column;
  overflow-y: auto;
  height: 100%;
  position: relative;
}
.explorer-empty {
  padding: 12px;
  color: var(--fluent-text-secondary, #666);
  font-size: 13px;
}
.explorer-blank {
  flex: 1 1 auto;
  min-height: 48px;
}
.tree-row {
  display: flex;
  align-items: center;
  min-height: 24px;
}
.tree-row.context-target {
  background: color-mix(in srgb, var(--fluent-accent) 12%, transparent);
}
.tree-row.focused {
  outline: 1px solid var(--fluent-accent);
  outline-offset: -1px;
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
.inline-input {
  flex: 1 1 auto;
  min-width: 0;
  font: inherit;
  color: inherit;
  background: var(--fluent-bg);
  border: 1px solid var(--fluent-accent);
  border-radius: 3px;
  padding: 1px 5px;
}
.context-menu {
  position: fixed;
  z-index: 20;
  display: flex;
  flex-direction: column;
  min-width: 160px;
  padding: 4px;
  background: var(--fluent-surface);
  border: 1px solid var(--fluent-border);
  border-radius: 6px;
  box-shadow: var(--fluent-shadow);
}
.context-menu button {
  text-align: left;
  border: none;
  background: none;
  color: inherit;
  padding: 6px 10px;
  border-radius: 4px;
  cursor: pointer;
  font: inherit;
}
.context-menu button:hover {
  background: color-mix(in srgb, var(--fluent-muted) 12%, transparent);
}
.context-menu button.danger {
  color: var(--fluent-danger);
}
.context-menu-separator {
  height: 1px;
  margin: 4px 2px;
  background: var(--fluent-border);
}
</style>
