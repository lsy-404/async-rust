<script setup lang="ts">
import {
  computed,
  nextTick,
  onMounted,
  onUnmounted,
  reactive,
  ref,
  watch,
  type ComponentPublicInstance,
} from "vue";
import MarkdownIt from "markdown-it";
import { invoke, Channel } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import ModelConnections from "./components/ModelConnections.vue";
import {
  FluentButton,
  FluentDialog,
  FluentField,
  FluentNotice,
  FluentProgressBar,
  FluentSelect,
  FluentTextArea,
  FluentTheme,
} from "@platform-kit/fluent/vue";
import type {
  AppData,
  Material,
  Session,
  Settings,
  StreamEvent,
  SttStatus,
  RecordingEvent,
  Theme,
  Workspace,
} from "./types";
import { cancelStream, streamCommand } from "./workbench-commands";

const md = new MarkdownIt({ html: false, linkify: true, breaks: true });
const blankSettings: Settings = {
  providerId: "openai",
  model: "",
  theme: "system",
  language: "zh",
};
const data = ref<AppData>({
  workspaces: [],
  sessions: [],
  materials: [],
  settings: { ...blankSettings },
  providers: [],
});
const selectedWorkspaceId = ref("");
const selectedSessionId = ref("");
const selectedMaterialId = ref("");
const draft = ref("");
const notice = ref("");
const error = ref("");
const loading = ref(true);
const streaming = ref(false);
const summaryLoading = ref(false);
const summaryStream = ref("");
const transcriptionLoading = ref(false);
const stt = ref<SttStatus>({
  ready: false,
  modelName: "本地语音模型",
  modelPath: "",
  sizeBytes: 0,
});
const sttDownloading = ref(false);
const sttProgress = ref({ downloaded: 0, total: 0 });
const settingsOpen = ref(false);
const workspaceName = ref("");
const sessionTitle = ref("");
const sessionCreateOpen = ref(false);
const materialDraft = ref("");
const recording = ref(false);
const deleteTarget = ref<{
  kind: "workspace" | "session" | "material";
  id: string;
  label: string;
}>();
const recordingStarting = ref(false);
const recordingSessionId = ref("");
let recordingGeneration = 0;
const sidebarMode = ref<"session" | "knowledge">("session");
const searchQuery = ref("");
const activeSessionTab = ref<"chat" | "summary">("chat");
const sidebarOpen = ref(true);
const mainPanelRatio = ref(0.62);
const layoutRef = ref<HTMLElement>();
const transcriptScrollRef = ref<HTMLElement>();
const expandedWorkspaces = ref(new Set<string>());
const contextMenu = ref<
  | {
      x: number;
      y: number;
      kind: "workspace" | "session" | "material";
      id: string;
      label: string;
      workspaceId?: string;
    }
  | undefined
>();
const contextMenuRef = ref<HTMLElement>();
const renamingId = ref("");
const renameKind = ref<"workspace" | "session" | "material">("workspace");
const renameDraft = ref("");
const sessionCreateWorkspaceId = ref("");
const themeChoices: { value: Theme; label: string }[] = [
  { value: "system", label: "系统" },
  { value: "light", label: "浅色" },
  { value: "dark", label: "深色" },
];

const workspaces = computed(() => data.value.workspaces);
const sessions = computed(() =>
  data.value.sessions.filter(
    (item) => item.workspaceId === selectedWorkspaceId.value,
  ),
);
const materials = computed(() =>
  data.value.materials.filter(
    (item) => item.workspaceId === selectedWorkspaceId.value,
  ),
);
const session = computed(() =>
  data.value.sessions.find((item) => item.id === selectedSessionId.value),
);
const material = computed(() =>
  data.value.materials.find((item) => item.id === selectedMaterialId.value),
);
const currentProvider = computed(() =>
  data.value.providers.find(
    (item) => item.id === data.value.settings.providerId,
  ),
);
const normalizedSearch = computed(() => searchQuery.value.trim().toLowerCase());
const filteredWorkspaces = computed(() => {
  const term = normalizedSearch.value;
  if (!term) return workspaces.value;
  return workspaces.value.filter(
    (workspace) =>
      workspace.name.toLowerCase().includes(term) ||
      data.value.sessions.some(
        (item) =>
          item.workspaceId === workspace.id &&
          item.title.toLowerCase().includes(term),
      ),
  );
});
const filteredMaterials = computed(() => {
  const term = normalizedSearch.value;
  if (!term) return materials.value;
  return materials.value.filter((item) =>
    item.name.toLowerCase().includes(term),
  );
});
const workspaceOptions = computed(() =>
  workspaces.value.map((workspace) => ({
    value: workspace.id,
    label: workspace.name,
  })),
);
const operationBusy = computed(
  () =>
    streaming.value ||
    summaryLoading.value ||
    transcriptionLoading.value ||
    recording.value ||
    recordingStarting.value,
);
const renderedMessages = computed(
  () =>
    session.value?.messages.map((message) => ({
      ...message,
      html: md.render(message.content),
    })) ?? [],
);
const deleteCopy = computed(() => {
  const target = deleteTarget.value;
  if (!target) return { title: "确认删除", body: "", confirmLabel: "删除" };
  if (target.kind === "workspace")
    return {
      title: "删除工作区",
      body: `删除工作区"${target.label}"会同时删除其中的全部会话与材料，且无法恢复。继续吗？`,
      confirmLabel: "删除工作区",
    };
  if (target.kind === "session")
    return {
      title: "删除会话",
      body: `删除会话"${target.label}"后，其中的对话与转写记录将无法恢复。继续吗？`,
      confirmLabel: "删除会话",
    };
  return {
    title: "删除学习资料",
    body: `删除学习资料"${target.label}"后无法恢复。继续吗？`,
    confirmLabel: "删除学习资料",
  };
});

function report(message: unknown) {
  error.value = String(message);
}
async function refresh() {
  loading.value = true;
  error.value = "";
  try {
    data.value = await invoke<AppData>("load_state");
    stt.value = await invoke<SttStatus>("stt_status");
    if (
      !selectedWorkspaceId.value ||
      !data.value.workspaces.some(
        (item) => item.id === selectedWorkspaceId.value,
      )
    )
      selectedWorkspaceId.value = data.value.workspaces[0]?.id ?? "";
    if (
      !selectedSessionId.value ||
      !sessions.value.some((item) => item.id === selectedSessionId.value)
    )
      selectedSessionId.value = sessions.value[0]?.id ?? "";
    if (
      !selectedMaterialId.value ||
      !materials.value.some((item) => item.id === selectedMaterialId.value)
    )
      selectedMaterialId.value = materials.value[0]?.id ?? "";
  } catch (cause) {
    report(cause);
  } finally {
    loading.value = false;
  }
}
async function refreshModelSettings() {
  const updated = await invoke<AppData>("load_state");
  data.value.providers = updated.providers;
  data.value.settings.providerId = updated.settings.providerId;
  data.value.settings.model = updated.settings.model;
}
async function createWorkspace() {
  if (!workspaceName.value.trim() || operationBusy.value) return;
  try {
    const workspace = await invoke<Workspace>("create_workspace", {
      name: workspaceName.value.trim(),
    });
    workspaceName.value = "";
    selectedWorkspaceId.value = workspace.id;
    await refresh();
  } catch (cause) {
    report(cause);
  }
}
function askDelete(
  kind: "workspace" | "session" | "material",
  id: string,
  label: string,
) {
  deleteTarget.value = { kind, id, label };
}
async function confirmDelete() {
  const target = deleteTarget.value;
  if (!target) return;
  try {
    if (target.kind === "workspace")
      await invoke("delete_workspace", { id: target.id });
    if (target.kind === "session")
      await invoke("delete_session", { id: target.id });
    if (target.kind === "material")
      await invoke("delete_material", { id: target.id });
    deleteTarget.value = undefined;
    await refresh();
  } catch (cause) {
    report(cause);
  }
}
function toggleWorkspaceExpand(id: string) {
  if (expandedWorkspaces.value.has(id)) expandedWorkspaces.value.delete(id);
  else expandedWorkspaces.value.add(id);
}
function openContextMenu(
  event: MouseEvent,
  kind: "workspace" | "session" | "material",
  id: string,
  label: string,
  workspaceId?: string,
) {
  contextMenu.value = {
    x: event.clientX,
    y: event.clientY,
    kind,
    id,
    label,
    workspaceId,
  };
  void nextTick(() => {
    const el = contextMenuRef.value;
    const current = contextMenu.value;
    if (!el || !current) return;
    const rect = el.getBoundingClientRect();
    contextMenu.value = {
      ...current,
      x: Math.min(current.x, Math.max(4, window.innerWidth - rect.width - 4)),
      y: Math.min(current.y, Math.max(4, window.innerHeight - rect.height - 4)),
    };
  });
}
function closeContextMenu() {
  contextMenu.value = undefined;
}
function contextMenuNewSession() {
  const menu = contextMenu.value;
  if (!menu || menu.kind !== "workspace") return;
  sessionCreateWorkspaceId.value = menu.id;
  sessionCreateOpen.value = true;
  closeContextMenu();
}
function contextMenuRename() {
  const menu = contextMenu.value;
  if (!menu) return;
  startRename(menu.kind, menu.id, menu.label);
  closeContextMenu();
}
function contextMenuDelete() {
  const menu = contextMenu.value;
  if (!menu) return;
  askDelete(menu.kind, menu.id, menu.label);
  closeContextMenu();
}
function startRename(
  kind: "workspace" | "session" | "material",
  id: string,
  currentName: string,
) {
  renameKind.value = kind;
  renamingId.value = id;
  renameDraft.value = currentName;
}
function cancelRename() {
  renamingId.value = "";
}
function updateRenameDraft(event: Event) {
  renameDraft.value = (event.target as HTMLInputElement).value;
}
function focusElement(el: Element | ComponentPublicInstance | null) {
  if (el instanceof HTMLInputElement) el.focus();
}
async function commitRename() {
  const id = renamingId.value;
  const kind = renameKind.value;
  const name = renameDraft.value.trim();
  renamingId.value = "";
  if (!id || !name) return;
  try {
    if (kind === "workspace") {
      const current = data.value.workspaces.find((item) => item.id === id);
      if (!current || current.name === name) return;
      await invoke("rename_workspace", { id, name });
    } else if (kind === "session") {
      const current = data.value.sessions.find((item) => item.id === id);
      if (!current || current.title === name) return;
      await invoke("save_session", { session: { ...current, title: name } });
    } else {
      const current = data.value.materials.find((item) => item.id === id);
      if (!current || current.name === name) return;
      await invoke("save_material", { material: { ...current, name } });
    }
    await refresh();
  } catch (cause) {
    report(cause);
  }
}
async function setTheme(theme: Theme) {
  if (data.value.settings.theme === theme) return;
  data.value.settings.theme = theme;
  try {
    await invoke("save_settings", { settings: data.value.settings });
  } catch (cause) {
    report(cause);
  }
}
function handleWindowClick() {
  if (contextMenu.value) closeContextMenu();
}
function handleWindowScroll() {
  if (contextMenu.value) closeContextMenu();
}
function handleWindowKeydown(event: KeyboardEvent) {
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "b") {
    event.preventDefault();
    sidebarOpen.value = !sidebarOpen.value;
    return;
  }
  if (event.key === "Escape") {
    if (contextMenu.value) closeContextMenu();
    if (renamingId.value) cancelRename();
  }
}
async function createSession() {
  const workspaceId =
    sessionCreateWorkspaceId.value || selectedWorkspaceId.value;
  if (!workspaceId || !sessionTitle.value.trim()) return;
  try {
    const created = await invoke<Session>("create_session", {
      workspaceId,
      title: sessionTitle.value.trim(),
    });
    sessionTitle.value = "";
    selectedWorkspaceId.value = workspaceId;
    selectedSessionId.value = created.id;
    await refresh();
  } catch (cause) {
    report(cause);
  }
}
function createQuickSession() {
  if (!workspaces.value.length) return;
  sessionCreateWorkspaceId.value =
    selectedWorkspaceId.value || workspaces.value[0]!.id;
  sessionCreateOpen.value = true;
}
function selectWorkspace(workspaceId: string) {
  if (operationBusy.value) return;
  selectedWorkspaceId.value = workspaceId;
  if (session.value?.workspaceId !== workspaceId) {
    selectedSessionId.value = sessions.value[0]?.id ?? "";
  }
  selectedMaterialId.value = materials.value[0]?.id ?? "";
}
function selectSession(workspaceId: string, sessionId: string) {
  if (operationBusy.value) return;
  selectedWorkspaceId.value = workspaceId;
  selectedSessionId.value = sessionId;
}
async function importMaterial() {
  if (!selectedWorkspaceId.value) return;
  try {
    const path = await open({
      multiple: false,
      title: "Choose a learning material",
      filters: [
        { name: "Learning material", extensions: ["txt", "md", "docx"] },
      ],
    });
    if (typeof path === "string") {
      const item = await invoke<Material>("import_material", {
        workspaceId: selectedWorkspaceId.value,
        path,
      });
      selectedMaterialId.value = item.id;
      await refresh();
    }
  } catch (cause) {
    report(cause);
  }
}
async function saveMaterial() {
  if (material.value)
    try {
      await invoke("save_material", {
        material: { ...material.value, content: materialDraft.value },
      });
      notice.value = "Material saved locally.";
      await refresh();
    } catch (cause) {
      report(cause);
    }
}
async function send() {
  if (
    !session.value ||
    !draft.value.trim() ||
    streaming.value ||
    summaryLoading.value ||
    transcriptionLoading.value
  )
    return;
  const target = session.value;
  const sessionId = target.id;
  const content = draft.value.trim();
  draft.value = "";
  streaming.value = true;
  error.value = "";
  const pending = reactive({
    id: `streaming-${sessionId}`,
    role: "assistant" as const,
    content: "",
  });
  target.messages.push(
    { id: crypto.randomUUID(), role: "user", content },
    pending,
  );
  const channel = new Channel<StreamEvent>();
  channel.onmessage = (event) => {
    if (event.type === "delta") pending.content += event.text;
  };
  try {
    await streamCommand(invoke, "chat", sessionId, channel, content);
    await refresh();
  } catch (cause) {
    target.messages = target.messages.filter((item) => item.id !== pending.id);
    report(cause);
  } finally {
    streaming.value = false;
  }
}
async function cancel() {
  if (session.value)
    try {
      await cancelStream(invoke, session.value.id);
    } catch (cause) {
      report(cause);
    }
}
async function summarize() {
  if (
    !session.value ||
    summaryLoading.value ||
    streaming.value ||
    transcriptionLoading.value
  )
    return;
  const sessionId = session.value.id;
  summaryLoading.value = true;
  summaryStream.value = "";
  const channel = new Channel<StreamEvent>();
  channel.onmessage = (event) => {
    if (event.type === "delta") summaryStream.value += event.text;
  };
  try {
    await streamCommand(invoke, "summarize", sessionId, channel);
    await refresh();
  } catch (cause) {
    report(cause);
  } finally {
    summaryLoading.value = false;
    summaryStream.value = "";
  }
}
async function uploadAudio() {
  if (
    !session.value ||
    streaming.value ||
    summaryLoading.value ||
    transcriptionLoading.value
  )
    return;
  const sessionId = session.value.id;
  transcriptionLoading.value = true;
  try {
    const path = await open({
      multiple: false,
      title: "Choose an audio recording",
      filters: [
        {
          name: "Audio",
          extensions: ["wav", "mp3", "flac", "ogg", "m4a", "aac"],
        },
      ],
    });
    if (typeof path === "string") {
      await invoke("transcribe_audio", { sessionId, path });
      await refresh();
    }
  } catch (cause) {
    report(cause);
  } finally {
    transcriptionLoading.value = false;
  }
}
async function downloadStt() {
  if (sttDownloading.value || stt.value.ready) return;
  sttDownloading.value = true;
  const channel = new Channel<{ downloaded: number; total: number }>();
  channel.onmessage = (event) => {
    sttProgress.value = event;
  };
  try {
    stt.value = await invoke<SttStatus>("download_stt_model", {
      onEvent: channel,
    });
  } catch (cause) {
    report(cause);
  } finally {
    sttDownloading.value = false;
  }
}
async function cancelStt() {
  try {
    await invoke("cancel_stt_download");
  } catch (cause) {
    report(cause);
  }
}
async function toggleRecording() {
  if (
    !session.value ||
    streaming.value ||
    summaryLoading.value ||
    transcriptionLoading.value ||
    recordingStarting.value
  )
    return;
  if (recording.value) {
    const sessionId = recordingSessionId.value;
    recording.value = false;
    transcriptionLoading.value = true;
    try {
      await invoke("stop_recording", { sessionId });
      await refresh();
    } catch (cause) {
      report(cause);
    } finally {
      transcriptionLoading.value = false;
      recordingSessionId.value = "";
      recordingGeneration += 1;
    }
    return;
  }
  const sessionId = session.value.id;
  recordingStarting.value = true;
  error.value = "";
  const generation = ++recordingGeneration;
  const onEvent = new Channel<RecordingEvent>();
  recordingSessionId.value = sessionId;
  onEvent.onmessage = (event) => {
    if (generation !== recordingGeneration || event.sessionId !== sessionId)
      return;
    if (event.type === "error") {
      report(event.text);
      recording.value = false;
      recordingSessionId.value = "";
      recordingGeneration += 1;
      void invoke("cancel_recording").catch(report);
      return;
    }
    const target = data.value.sessions.find((item) => item.id === sessionId);
    if (target) target.transcription = event.text;
  };
  try {
    await invoke("start_recording", { sessionId, onEvent });
    if (generation !== recordingGeneration) return;
    recording.value = true;
  } catch (cause) {
    recordingSessionId.value = "";
    recordingGeneration += 1;
    report(cause);
  } finally {
    recordingStarting.value = false;
  }
}
async function saveSettings() {
  try {
    await invoke("save_settings", { settings: data.value.settings });
    settingsOpen.value = false;
    notice.value = "Settings saved locally.";
    await refresh();
  } catch (cause) {
    report(cause);
  }
}
function handleComposerEnter(event: KeyboardEvent) {
  if (!event.isComposing) void send();
}
function beginResize(event: PointerEvent) {
  const layout = layoutRef.value;
  if (!layout) return;
  const bounds = layout.getBoundingClientRect();
  const startX = event.clientX;
  const initial = Math.round(bounds.width * mainPanelRatio.value);
  const onMove = (move: PointerEvent) => {
    const next = initial + (move.clientX - startX);
    mainPanelRatio.value = Math.max(0.42, Math.min(0.7, next / bounds.width));
  };
  const onEnd = () => {
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onEnd);
  };
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onEnd, { once: true });
}
function resizeWithKeyboard(event: KeyboardEvent) {
  if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
  event.preventDefault();
  const width = layoutRef.value?.clientWidth ?? 900;
  const current = Math.round(width * mainPanelRatio.value);
  const delta = event.key === "ArrowLeft" ? -24 : 24;
  mainPanelRatio.value = Math.max(
    0.42,
    Math.min(0.7, (current + delta) / width),
  );
}
watch(selectedWorkspaceId, () => {
  if (
    !operationBusy.value &&
    session.value?.workspaceId !== selectedWorkspaceId.value
  ) {
    selectedSessionId.value = sessions.value[0]?.id ?? "";
  }
  if (!operationBusy.value) {
    selectedMaterialId.value = materials.value[0]?.id ?? "";
  }
});
watch(
  material,
  (value) => {
    materialDraft.value = value?.content ?? "";
  },
  { immediate: true },
);
watch(
  () => data.value.settings.theme,
  (theme) =>
    (document.documentElement.dataset.fluentTheme =
      theme === "system" ? "" : theme),
  { immediate: true },
);
watch(
  () => data.value.workspaces.map((item) => item.id),
  (ids, previousIds) => {
    const seen = new Set(previousIds ?? []);
    for (const id of ids) {
      if (!seen.has(id)) expandedWorkspaces.value.add(id);
    }
  },
  { immediate: true },
);
watch(
  () => session.value?.transcription,
  async () => {
    const panel = transcriptScrollRef.value;
    if (!panel) return;
    const distanceFromBottom =
      panel.scrollHeight - panel.scrollTop - panel.clientHeight;
    if (distanceFromBottom > 36) return;
    await nextTick();
    panel.scrollTop = panel.scrollHeight;
  },
);
onMounted(() => {
  void refresh();
  window.addEventListener("click", handleWindowClick);
  window.addEventListener("keydown", handleWindowKeydown);
  window.addEventListener("scroll", handleWindowScroll, true);
});
onUnmounted(() => {
  window.removeEventListener("click", handleWindowClick);
  window.removeEventListener("keydown", handleWindowKeydown);
  window.removeEventListener("scroll", handleWindowScroll, true);
  recordingGeneration += 1;
  if (recording.value || recordingStarting.value)
    void invoke("cancel_recording").catch(() => undefined);
});
</script>

<template>
  <FluentTheme :mode="data.settings.theme">
    <main class="app-shell">
      <FluentNotice v-if="error" tone="danger" class="notice"
        >{{ error }} <button @click="error = ''">关闭</button></FluentNotice
      >
      <FluentNotice v-if="notice" tone="success" class="notice"
        >{{ notice }} <button @click="notice = ''">关闭</button></FluentNotice
      >
      <div v-if="loading" class="loading">正在加载本地课堂数据…</div>
      <div v-else class="desktop-shell">
        <aside v-if="sidebarOpen" class="app-sidebar">
          <div class="sidebar-search">
            <FluentField
              v-model="searchQuery"
              label="搜索"
              placeholder="搜索工作区、会话或知识库"
            />
          </div>
          <div class="sidebar-tabs">
            <FluentButton
              :class="{ active: sidebarMode === 'session' }"
              tone="subtle"
              @click="sidebarMode = 'session'"
              >会话</FluentButton
            ><FluentButton
              :class="{ active: sidebarMode === 'knowledge' }"
              tone="subtle"
              @click="sidebarMode = 'knowledge'"
              >知识库</FluentButton
            >
          </div>
          <div v-if="sidebarMode === 'session'" class="sidebar-tree">
            <p v-if="!filteredWorkspaces.length" class="empty">
              {{
                normalizedSearch
                  ? "未找到匹配的工作区或会话"
                  : "创建一个工作区开始整理课堂。"
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
                  openContextMenu(
                    $event,
                    'workspace',
                    workspace.id,
                    workspace.name,
                  )
                "
              >
                <button
                  type="button"
                  class="tree-expand"
                  :aria-expanded="expandedWorkspaces.has(workspace.id)"
                  aria-label="展开或折叠工作区"
                  @click.stop="toggleWorkspaceExpand(workspace.id)"
                >
                  {{ expandedWorkspaces.has(workspace.id) ? "▾" : "▸" }}
                </button>
                <input
                  v-if="renamingId === workspace.id"
                  class="rename-input"
                  :value="renameDraft"
                  :ref="focusElement"
                  @input="updateRenameDraft"
                  @keydown.enter.prevent="commitRename"
                  @keydown.escape.prevent="cancelRename"
                  @blur="commitRename"
                  @click.stop
                />
                <button
                  v-else
                  class="tree-workspace"
                  :class="{ active: workspace.id === selectedWorkspaceId }"
                  :disabled="operationBusy"
                  @click="selectWorkspace(workspace.id)"
                  @dblclick="
                    startRename('workspace', workspace.id, workspace.name)
                  "
                >
                  {{ workspace.name }}
                </button>
                <FluentButton
                  class="tree-delete"
                  tone="subtle"
                  :disabled="operationBusy"
                  :aria-label="`删除工作区 ${workspace.name}`"
                  @click="askDelete('workspace', workspace.id, workspace.name)"
                  >删除</FluentButton
                >
              </div>
              <template v-if="expandedWorkspaces.has(workspace.id)">
                <div
                  v-for="item in data.sessions.filter(
                    (candidate) =>
                      candidate.workspaceId === workspace.id &&
                      (!normalizedSearch ||
                        candidate.title
                          .toLowerCase()
                          .includes(normalizedSearch)),
                  )"
                  :key="item.id"
                  class="tree-row"
                  @contextmenu.prevent="
                    openContextMenu(
                      $event,
                      'session',
                      item.id,
                      item.title,
                      workspace.id,
                    )
                  "
                >
                  <input
                    v-if="renamingId === item.id"
                    class="rename-input"
                    :value="renameDraft"
                    :ref="focusElement"
                    @input="updateRenameDraft"
                    @keydown.enter.prevent="commitRename"
                    @keydown.escape.prevent="cancelRename"
                    @blur="commitRename"
                    @click.stop
                  />
                  <button
                    v-else
                    class="tree-session"
                    :class="{ active: item.id === selectedSessionId }"
                    :disabled="operationBusy"
                    @click="selectSession(workspace.id, item.id)"
                    @dblclick="startRename('session', item.id, item.title)"
                  >
                    ▢ {{ item.title }}
                  </button>
                  <FluentButton
                    class="tree-delete"
                    tone="subtle"
                    :disabled="operationBusy"
                    :aria-label="`删除会话 ${item.title}`"
                    @click="askDelete('session', item.id, item.title)"
                    >删除</FluentButton
                  >
                </div>
              </template>
            </section>
          </div>
          <div v-else class="sidebar-tree knowledge-tree">
            <div class="knowledge-head">
              <FluentSelect
                v-model="selectedWorkspaceId"
                label="工作区"
                :options="workspaceOptions"
                :disabled="operationBusy"
              />
              <span>本地材料</span
              ><FluentButton
                tone="subtle"
                :disabled="!selectedWorkspaceId"
                @click="importMaterial"
                >导入</FluentButton
              >
            </div>
            <p v-if="!selectedWorkspaceId" class="empty">先选择一个工作区。</p>
            <p v-else-if="!filteredMaterials.length" class="empty">
              {{
                normalizedSearch
                  ? "未找到匹配的材料"
                  : "导入 TXT、Markdown 或 DOCX 材料。"
              }}
            </p>
            <div
              v-for="item in filteredMaterials"
              :key="item.id"
              class="tree-row"
              @contextmenu.prevent="
                openContextMenu($event, 'material', item.id, item.name)
              "
            >
              <input
                v-if="renamingId === item.id"
                class="rename-input"
                :value="renameDraft"
                :ref="focusElement"
                @input="updateRenameDraft"
                @keydown.enter.prevent="commitRename"
                @keydown.escape.prevent="cancelRename"
                @blur="commitRename"
                @click.stop
              />
              <button
                v-else
                class="tree-session"
                :class="{ active: item.id === selectedMaterialId }"
                :disabled="operationBusy"
                @click="selectedMaterialId = item.id"
                @dblclick="startRename('material', item.id, item.name)"
              >
                ▤ {{ item.name }}
              </button>
              <FluentButton
                class="tree-delete"
                tone="subtle"
                :disabled="operationBusy"
                :aria-label="`删除材料 ${item.name}`"
                @click="askDelete('material', item.id, item.name)"
                >删除</FluentButton
              >
            </div>
          </div>
          <footer class="sidebar-footer" v-if="sidebarMode === 'session'">
            <FluentButton
              tone="subtle"
              :disabled="!workspaces.length || operationBusy"
              @click="createQuickSession"
              >新建会话</FluentButton
            >
            <form @submit.prevent="createWorkspace">
              <FluentField
                v-model="workspaceName"
                label="工作区"
                placeholder="新工作区"
              /><FluentButton
                type="submit"
                tone="subtle"
                :disabled="operationBusy"
                >新建工作区</FluentButton
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
              @click="importMaterial"
              >导入材料</FluentButton
            >
          </footer>
        </aside>
        <section
          ref="layoutRef"
          class="workbench-layout"
          :style="
            session
              ? {
                  gridTemplateColumns: `minmax(0, ${mainPanelRatio}fr) 8px minmax(0, ${1 - mainPanelRatio}fr)`,
                }
              : { gridTemplateColumns: '1fr' }
          "
        >
          <header class="app-header">
            <FluentButton
              tone="subtle"
              :aria-expanded="sidebarOpen"
              aria-label="切换侧栏"
              @click="sidebarOpen = !sidebarOpen"
              ><svg
                width="16"
                height="16"
                viewBox="0 0 16 16"
                fill="none"
                aria-hidden="true"
              >
                <path
                  d="M2 4h12M2 8h12M2 12h12"
                  stroke="currentColor"
                  stroke-width="1.4"
                  stroke-linecap="round"
                /></svg
            ></FluentButton>
            <h1>{{ session?.title || "Async" }}</h1>
            <div class="header-actions">
              <div class="header-switches" role="group" aria-label="切换主题">
                <button
                  v-for="choice in themeChoices"
                  :key="choice.value"
                  type="button"
                  :class="{ active: data.settings.theme === choice.value }"
                  @click="setTheme(choice.value)"
                >
                  {{ choice.label }}
                </button>
              </div>
              <span
                :class="[
                  'status',
                  currentProvider?.hasKey ? 'status-ok' : 'status-warn',
                ]"
                >{{
                  currentProvider?.hasKey
                    ? currentProvider.name
                    : "需要连接模型"
                }}</span
              ><FluentButton tone="subtle" @click="settingsOpen = true"
                >设置</FluentButton
              >
            </div>
          </header>
          <template v-if="session">
            <section class="main-workspace">
              <div class="session-tabs">
                <FluentButton
                  :class="{ active: activeSessionTab === 'chat' }"
                  tone="subtle"
                  @click="activeSessionTab = 'chat'"
                  >对话</FluentButton
                ><FluentButton
                  :class="{ active: activeSessionTab === 'summary' }"
                  tone="subtle"
                  @click="activeSessionTab = 'summary'"
                  >摘要</FluentButton
                >
              </div>
              <template v-if="activeSessionTab === 'chat'"
                ><div class="messages">
                  <article
                    v-for="message in renderedMessages"
                    :key="message.id"
                    :class="['message', message.role]"
                  >
                    <label>{{
                      message.role === "user" ? "你" : "课堂助手"
                    }}</label>
                    <div class="message-body" v-html="message.html"></div>
                  </article>
                  <div v-if="!renderedMessages.length" class="empty center">
                    向课堂助手提问，它会结合当前工作区的材料和转写内容。
                  </div>
                </div>
                <form class="composer" @submit.prevent="send">
                  <FluentTextArea
                    v-model="draft"
                    label="提问"
                    :disabled="operationBusy"
                    placeholder="输入问题，Enter 发送，Shift+Enter 换行"
                    @keydown.enter.exact.prevent="handleComposerEnter"
                  /><FluentButton v-if="streaming" tone="danger" @click="cancel"
                    >停止</FluentButton
                  ><FluentButton
                    v-else
                    tone="primary"
                    type="submit"
                    :disabled="
                      !draft.trim() || !currentProvider?.hasKey || operationBusy
                    "
                    >发送</FluentButton
                  >
                </form></template
              >
              <section v-else class="summary-page">
                <header>
                  <div>
                    <h2>课堂摘要</h2>
                    <p>根据当前对话、材料与转写生成。</p>
                  </div>
                  <FluentButton
                    :tone="summaryLoading ? 'danger' : 'secondary'"
                    :disabled="operationBusy && !summaryLoading"
                    @click="summaryLoading ? cancel() : summarize()"
                    >{{
                      summaryLoading ? "停止摘要" : "生成摘要"
                    }}</FluentButton
                  >
                </header>
                <div v-if="summaryLoading && !summaryStream" class="empty">
                  正在生成摘要…
                </div>
                <div
                  v-else-if="session.summary || summaryStream"
                  class="summary-content"
                  v-html="md.render(summaryStream || session.summary || '')"
                ></div>
                <div v-else class="empty">还没有课堂摘要。</div>
              </section>
            </section>
            <div
              class="resize-handle"
              role="separator"
              tabindex="0"
              aria-label="调整转写面板宽度"
              aria-orientation="vertical"
              :aria-valuenow="Math.round(mainPanelRatio * 100)"
              @pointerdown="beginResize"
              @keydown="resizeWithKeyboard"
            ></div>
            <aside class="transcript-panel">
              <header>
                <div>
                  <h2>课堂转写</h2>
                  <p>
                    {{
                      stt.ready ? "本地模型已就绪" : "下载本地模型后可离线转写"
                    }}
                  </p>
                </div>
                <FluentButton
                  tone="subtle"
                  :busy="transcriptionLoading"
                  :disabled="!session || operationBusy"
                  @click="uploadAudio"
                  >导入音频</FluentButton
                >
              </header>
              <div ref="transcriptScrollRef" class="transcript-content">
                <p v-if="session?.transcription">{{ session.transcription }}</p>
                <div v-else class="empty transcript-empty">
                  录音或导入音频后，转写会持续显示在这里。
                </div>
              </div>
              <footer class="recording-bar">
                <span class="recording-state">{{
                  recordingStarting
                    ? "正在加载本地模型…"
                    : recording
                      ? "正在本地录音与转写…"
                      : transcriptionLoading
                        ? "正在完成转写…"
                        : "准备就绪"
                }}</span
                ><FluentButton
                  v-if="transcriptionLoading && !recordingSessionId"
                  tone="danger"
                  @click="cancel"
                  >取消转写</FluentButton
                ><FluentButton
                  :tone="recording ? 'danger' : 'primary'"
                  :busy="recordingStarting"
                  :disabled="
                    !session ||
                    streaming ||
                    summaryLoading ||
                    transcriptionLoading
                  "
                  @click="toggleRecording"
                  >{{ recording ? "停止录音" : "开始录音" }}</FluentButton
                >
              </footer>
            </aside>
          </template>
          <section v-else class="main-workspace">
            <div v-if="material" class="material-editor-panel">
              <h2>{{ material.name }}</h2>
              <FluentTextArea
                v-model="materialDraft"
                label="材料内容"
                class="material-editor"
              />
              <div class="material-actions">
                <FluentButton tone="secondary" @click="saveMaterial"
                  >保存</FluentButton
                ><FluentButton
                  tone="danger"
                  @click="askDelete('material', material.id, material.name)"
                  >删除</FluentButton
                >
              </div>
            </div>
            <div v-else class="workbench-empty">
              <h2>选择或创建一个会话</h2>
              <p>
                选择或创建一个会话以开始课堂对话，或在知识库中选择一份材料进行编辑。
              </p>
            </div>
          </section>
        </section>
      </div>
      <div
        v-if="contextMenu"
        ref="contextMenuRef"
        class="context-menu"
        :style="{ left: contextMenu.x + 'px', top: contextMenu.y + 'px' }"
        @click.stop
      >
        <button
          v-if="contextMenu.kind === 'workspace'"
          type="button"
          @click="contextMenuNewSession"
        >
          新建会话
        </button>
        <button type="button" @click="contextMenuRename">重命名</button>
        <button type="button" class="danger" @click="contextMenuDelete">
          删除
        </button>
      </div>
      <FluentDialog v-model:open="sessionCreateOpen" label="新建会话">
        <template #title><h2>新建会话</h2></template>
        <template #default>
          <p class="dialog-description">
            将在下方选择的工作区中创建一个新的课堂会话。
          </p>
          <FluentSelect
            v-model="sessionCreateWorkspaceId"
            label="工作区"
            :options="workspaceOptions"
          />
          <FluentField
            v-model="sessionTitle"
            label="会话标题"
            placeholder="例如：第 3 课讨论"
          />
        </template>
        <template #footer
          ><FluentButton tone="subtle" @click="sessionCreateOpen = false"
            >取消</FluentButton
          ><FluentButton
            tone="primary"
            :disabled="!sessionTitle.trim() || !sessionCreateWorkspaceId"
            @click="
              createSession();
              sessionCreateOpen = false;
            "
            >新建</FluentButton
          ></template
        >
      </FluentDialog>
      <FluentDialog v-model:open="settingsOpen" label="设置"
        ><template #title><h2>设置</h2></template>
        <template #default
          ><div class="settings">
            <ModelConnections
              :theme="data.settings.theme"
              :refresh-workbench="refreshModelSettings"
            />
            <section class="stt-status">
              <strong>本地语音转写</strong>
              <p>
                {{
                  stt.ready
                    ? `已就绪：${stt.modelName}`
                    : "下载本地模型后可离线转写音频，无需 API Key。"
                }}
              </p>
              <FluentProgressBar
                v-if="sttDownloading"
                label="本地语音模型下载进度"
                :indeterminate="!sttProgress.total"
                :value="sttProgress.downloaded"
                :max="sttProgress.total || 1"
              /><FluentButton
                v-if="sttDownloading"
                tone="danger"
                @click="cancelStt"
                >取消下载</FluentButton
              ><FluentButton
                v-else
                tone="secondary"
                :disabled="stt.ready"
                @click="downloadStt"
                >{{
                  stt.ready ? "本地模型已就绪" : "下载本地转写模型"
                }}</FluentButton
              >
            </section>
            <FluentSelect
              v-model="data.settings.theme"
              label="主题"
              :options="[
                { value: 'system', label: '跟随系统' },
                { value: 'light', label: '浅色' },
                { value: 'dark', label: '深色' },
              ]"
            /></div></template
        ><template #footer
          ><FluentButton tone="subtle" @click="settingsOpen = false"
            >关闭</FluentButton
          ><FluentButton tone="primary" @click="saveSettings"
            >保存设置</FluentButton
          ></template
        ></FluentDialog
      >
      <FluentDialog
        :open="Boolean(deleteTarget)"
        :label="deleteCopy.title"
        @update:open="
          (open) => {
            if (!open) deleteTarget = undefined;
          }
        "
        ><template #title
          ><h2>{{ deleteCopy.title }}</h2></template
        >
        <template #default
          ><p>{{ deleteCopy.body }}</p></template
        >
        <template #footer
          ><FluentButton tone="subtle" @click="deleteTarget = undefined"
            >取消</FluentButton
          ><FluentButton tone="danger" @click="confirmDelete">{{
            deleteCopy.confirmLabel
          }}</FluentButton></template
        ></FluentDialog
      >
    </main>
  </FluentTheme>
</template>
