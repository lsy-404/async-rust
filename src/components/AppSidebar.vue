<script setup lang="ts">
import type { ComponentPublicInstance } from "vue";
import { i18n } from "../locales";
import {
  FluentButton,
  FluentField,
  FluentSelect,
} from "@platform-kit/fluent/vue";
import type { Material, Session, Workspace } from "../types";

const { t } = i18n.global;

defineProps<{
  searchQuery: string;
  sidebarMode: "session" | "knowledge";
  filteredWorkspaces: Workspace[];
  normalizedSearch: string;
  sessions: Session[];
  selectedWorkspaceId: string;
  selectedSessionId: string;
  selectedMaterialId: string;
  expandedWorkspaces: Set<string>;
  renamingId: string;
  renameDraft: string;
  operationBusy: boolean;
  filteredMaterials: Material[];
  workspaceOptions: { value: string; label: string }[];
  workspaces: Workspace[];
  workspaceName: string;
}>();
const emit = defineEmits<{
  "update:searchQuery": [value: string];
  "update:sidebarMode": [value: "session" | "knowledge"];
  "update:selectedWorkspaceId": [value: string];
  "update:renameDraft": [value: string];
  "update:workspaceName": [value: string];
  "toggle-workspace": [id: string];
  "context-menu": [
    payload: {
      event: MouseEvent;
      kind: "workspace" | "session" | "material";
      id: string;
      label: string;
      workspaceId?: string;
    },
  ];
  "select-workspace": [workspaceId: string];
  "select-session": [workspaceId: string, sessionId: string];
  "select-material": [id: string];
  "start-rename": [
    kind: "workspace" | "session" | "material",
    id: string,
    label: string,
  ];
  "cancel-rename": [];
  "commit-rename": [];
  "ask-delete": [
    kind: "workspace" | "session" | "material",
    id: string,
    label: string,
  ];
  "import-material": [];
  "create-quick-session": [];
  "create-workspace": [];
}>();

function focusElement(el: Element | ComponentPublicInstance | null) {
  if (el instanceof HTMLInputElement) el.focus();
}
function updateRenameDraft(event: Event) {
  emit("update:renameDraft", (event.target as HTMLInputElement).value);
}
</script>

<template>
  <aside class="app-sidebar">
    <div class="sidebar-search">
      <FluentField
        :model-value="searchQuery"
        :label="t('sidebar.search')"
        :placeholder="t('sidebar.searchPlaceholder')"
        @update:model-value="emit('update:searchQuery', $event)"
      />
    </div>
    <div class="sidebar-tabs">
      <FluentButton
        :class="{ active: sidebarMode === 'session' }"
        tone="subtle"
        @click="emit('update:sidebarMode', 'session')"
        >{{ t("sidebar.tabs.sessions") }}</FluentButton
      ><FluentButton
        :class="{ active: sidebarMode === 'knowledge' }"
        tone="subtle"
        @click="emit('update:sidebarMode', 'knowledge')"
        >{{ t("sidebar.tabs.knowledge") }}</FluentButton
      >
    </div>
    <div v-if="sidebarMode === 'session'" class="sidebar-tree">
      <p v-if="!filteredWorkspaces.length" class="empty">
        {{
          normalizedSearch
            ? t("sidebar.noSearchResults")
            : t("sidebar.emptyWorkspaces")
        }}
      </p>
      <section
        v-for="workspace in filteredWorkspaces"
        :key="workspace.id"
        class="workspace-node"
      >
        <div
          class="tree-row"
          @contextmenu.prevent="
            emit('context-menu', {
              event: $event,
              kind: 'workspace',
              id: workspace.id,
              label: workspace.name,
            })
          "
        >
          <button
            type="button"
            class="tree-expand"
            :aria-expanded="expandedWorkspaces.has(workspace.id)"
            :aria-label="t('sidebar.toggleWorkspaceAria')"
            @click.stop="emit('toggle-workspace', workspace.id)"
          >
            {{ expandedWorkspaces.has(workspace.id) ? "▾" : "▸" }}
          </button>
          <input
            v-if="renamingId === workspace.id"
            class="rename-input"
            :value="renameDraft"
            :ref="focusElement"
            @input="updateRenameDraft"
            @keydown.enter.prevent="emit('commit-rename')"
            @keydown.escape.prevent="emit('cancel-rename')"
            @blur="emit('commit-rename')"
            @click.stop
          />
          <button
            v-else
            class="tree-workspace"
            :class="{ active: workspace.id === selectedWorkspaceId }"
            :disabled="operationBusy"
            @click="emit('select-workspace', workspace.id)"
            @dblclick="emit('start-rename', 'workspace', workspace.id, workspace.name)"
          >
            {{ workspace.name }}
          </button>
          <FluentButton
            class="tree-delete"
            tone="subtle"
            :disabled="operationBusy"
            :aria-label="t('sidebar.deleteWorkspaceAria', { name: workspace.name })"
            @click="emit('ask-delete', 'workspace', workspace.id, workspace.name)"
            >{{ t("common.delete") }}</FluentButton
          >
        </div>
        <template v-if="expandedWorkspaces.has(workspace.id)">
          <div
            v-for="item in sessions.filter(
              (candidate) =>
                candidate.workspaceId === workspace.id &&
                (!normalizedSearch ||
                  candidate.title.toLowerCase().includes(normalizedSearch)),
            )"
            :key="item.id"
            class="tree-row"
            @contextmenu.prevent="
              emit('context-menu', {
                event: $event,
                kind: 'session',
                id: item.id,
                label: item.title,
                workspaceId: workspace.id,
              })
            "
          >
            <input
              v-if="renamingId === item.id"
              class="rename-input"
              :value="renameDraft"
              :ref="focusElement"
              @input="updateRenameDraft"
              @keydown.enter.prevent="emit('commit-rename')"
              @keydown.escape.prevent="emit('cancel-rename')"
              @blur="emit('commit-rename')"
              @click.stop
            />
            <button
              v-else
              class="tree-session"
              :class="{ active: item.id === selectedSessionId }"
              :disabled="operationBusy"
              @click="emit('select-session', workspace.id, item.id)"
              @dblclick="emit('start-rename', 'session', item.id, item.title)"
            >
              ▢ {{ item.title }}
            </button>
            <FluentButton
              class="tree-delete"
              tone="subtle"
              :disabled="operationBusy"
              :aria-label="t('sidebar.deleteSessionAria', { name: item.title })"
              @click="emit('ask-delete', 'session', item.id, item.title)"
              >{{ t("common.delete") }}</FluentButton
            >
          </div>
        </template>
      </section>
    </div>
    <div v-else class="sidebar-tree knowledge-tree">
      <div class="knowledge-head">
        <FluentSelect
          :model-value="selectedWorkspaceId"
          :label="t('sidebar.knowledge.workspaceLabel')"
          :options="workspaceOptions"
          :disabled="operationBusy"
          @update:model-value="emit('update:selectedWorkspaceId', $event)"
        />
        <span>{{ t("sidebar.knowledge.localMaterials") }}</span
        ><FluentButton
          tone="subtle"
          :disabled="!selectedWorkspaceId"
          @click="emit('import-material')"
          >{{ t("sidebar.knowledge.import") }}</FluentButton
        >
      </div>
      <p v-if="!selectedWorkspaceId" class="empty">{{ t("sidebar.knowledge.selectWorkspaceFirst") }}</p>
      <p v-else-if="!filteredMaterials.length" class="empty">
        {{
          normalizedSearch
            ? t("sidebar.knowledge.noSearchResults")
            : t("sidebar.knowledge.emptyMaterials")
        }}
      </p>
      <div
        v-for="item in filteredMaterials"
        :key="item.id"
        class="tree-row"
        @contextmenu.prevent="
          emit('context-menu', {
            event: $event,
            kind: 'material',
            id: item.id,
            label: item.name,
          })
        "
      >
        <input
          v-if="renamingId === item.id"
          class="rename-input"
          :value="renameDraft"
          :ref="focusElement"
          @input="updateRenameDraft"
          @keydown.enter.prevent="emit('commit-rename')"
          @keydown.escape.prevent="emit('cancel-rename')"
          @blur="emit('commit-rename')"
          @click.stop
        />
        <button
          v-else
          class="tree-session"
          :class="{ active: item.id === selectedMaterialId }"
          :disabled="operationBusy"
          @click="emit('select-material', item.id)"
          @dblclick="emit('start-rename', 'material', item.id, item.name)"
        >
          ▤ {{ item.name }}
        </button>
        <FluentButton
          class="tree-delete"
          tone="subtle"
          :disabled="operationBusy"
          :aria-label="t('sidebar.deleteMaterialAria', { name: item.name })"
          @click="emit('ask-delete', 'material', item.id, item.name)"
          >{{ t("common.delete") }}</FluentButton
        >
      </div>
    </div>
    <footer class="sidebar-footer" v-if="sidebarMode === 'session'">
      <FluentButton
        tone="subtle"
        :disabled="!workspaces.length || operationBusy"
        @click="emit('create-quick-session')"
        >{{ t("sidebar.createSession") }}</FluentButton
      >
      <form @submit.prevent="emit('create-workspace')">
        <FluentField
          :model-value="workspaceName"
          :label="t('sidebar.knowledge.workspaceLabel')"
          :placeholder="t('sidebar.newWorkspacePlaceholder')"
          @update:model-value="emit('update:workspaceName', $event)"
        /><FluentButton
          type="submit"
          tone="subtle"
          :disabled="operationBusy"
          >{{ t("sidebar.createWorkspace") }}</FluentButton
        >
      </form>
    </footer>
    <footer
      class="sidebar-footer"
      v-else-if="sidebarMode === 'knowledge'"
    >
      <FluentButton
        tone="subtle"
        :disabled="!selectedWorkspaceId"
        @click="emit('import-material')"
        >{{ t("sidebar.knowledge.import") }}</FluentButton
      >
    </footer>
  </aside>
</template>
