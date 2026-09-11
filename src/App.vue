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
import texmath from "markdown-it-texmath";
import katex from "katex";
import { invoke, Channel } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import ModelConnections from "./components/ModelConnections.vue";
import { i18n, setLocale } from "./locales";
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

const { t } = i18n.global;
const md = new MarkdownIt({ html: false, linkify: true, breaks: true }).use(
  texmath,
  {
    engine: katex,
    delimiters: "dollars",
    katexOptions: { throwOnError: false },
  },
);
// Fenced code blocks get a copy affordance; clicks are handled by delegation since this is raw v-html.
const defaultFenceRule =
  md.renderer.rules.fence ??
  ((tokens, idx, options, _env, self) =>
    self.renderToken(tokens, idx, options));
md.renderer.rules.fence = (tokens, idx, options, env, self) =>
  `<div class="code-block"><button type="button" class="code-copy" aria-label="${t("codeBlock.copyAria")}">${t("codeBlock.copy")}</button>${defaultFenceRule(tokens, idx, options, env, self)}</div>`;
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
  modelName: t("stt.defaultModelName"),
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
const sessionTabsById = reactive<Record<string, "chat" | "summary">>({});
const activeSessionTab = computed<"chat" | "summary">({
  get: () =>
    session.value ? (sessionTabsById[session.value.id] ?? "chat") : "chat",
  set: (value) => {
    if (session.value) sessionTabsById[session.value.id] = value;
  },
});
const sidebarOpen = ref(true);
const mainPanelRatio = ref(0.62);
let layoutSettingsInitialized = false;
const layoutRef = ref<HTMLElement>();
const transcriptScrollRef = ref<HTMLElement>();
const messagesRef = ref<HTMLElement>();
const composerTextareaEl = ref<HTMLTextAreaElement>();
const editingMessageId = ref("");
const editDraft = ref("");
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
const themeChoices = computed<{ value: Theme; label: string }[]>(() => [
  { value: "system", label: t("theme.system") },
  { value: "light", label: t("theme.light") },
  { value: "dark", label: t("theme.dark") },
]);

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
const summaryUpdatedLabel = computed(() => {
  const iso = session.value?.summaryUpdatedAt;
  if (!iso) return "";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  const locale = data.value.settings.language === "en" ? "en-US" : "zh-CN";
  return new Intl.DateTimeFormat(locale, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
});
const deleteCopy = computed(() => {
  const target = deleteTarget.value;
  if (!target)
    return {
      title: t("deleteDialog.title"),
      body: "",
      confirmLabel: t("common.delete"),
    };
  if (target.kind === "workspace")
    return {
      title: t("deleteWorkspace.title"),
      body: t("deleteWorkspace.body", { name: target.label }),
      confirmLabel: t("deleteWorkspace.confirm"),
    };
  if (target.kind === "session")
    return {
      title: t("deleteSession.title"),
      body: t("deleteSession.body", { name: target.label }),
      confirmLabel: t("deleteSession.confirm"),
    };
  return {
    title: t("deleteMaterial.title"),
    body: t("deleteMaterial.body", { name: target.label }),
    confirmLabel: t("deleteMaterial.confirm"),
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
    if (!layoutSettingsInitialized) {
      layoutSettingsInitialized = true;
      mainPanelRatio.value = data.value.settings.mainPanelRatio ?? 0.62;
      sidebarOpen.value = data.value.settings.sidebarOpen ?? true;
    }
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
  if (!target || operationBusy.value) return;
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
  if (operationBusy.value) return;
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
  if (!menu || operationBusy.value) return;
  startRename(menu.kind, menu.id, menu.label);
  closeContextMenu();
}
function contextMenuDelete() {
  const menu = contextMenu.value;
  if (!menu || operationBusy.value) return;
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
  if (!id || !name || operationBusy.value) return;
  try {
    if (kind === "workspace") {
      const current = data.value.workspaces.find((item) => item.id === id);
      if (!current || current.name === name) return;
      await invoke("rename_workspace", { id, name });
    } else if (kind === "session") {
      const current = data.value.sessions.find((item) => item.id === id);
      if (!current || current.title === name) return;
      await invoke("rename_session", { id, title: name });
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
async function setLanguage(language: Settings["language"]) {
  if (data.value.settings.language === language) return;
  data.value.settings.language = language;
  try {
    await invoke("save_settings", { settings: data.value.settings });
  } catch (cause) {
    report(cause);
  }
}
async function persistLayoutSettings() {
  data.value.settings.mainPanelRatio = mainPanelRatio.value;
  data.value.settings.sidebarOpen = sidebarOpen.value;
  try {
    await invoke("save_settings", { settings: data.value.settings });
  } catch (cause) {
    report(cause);
  }
}
function toggleSidebar() {
  sidebarOpen.value = !sidebarOpen.value;
  void persistLayoutSettings();
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
    toggleSidebar();
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
      title: t("materialPicker.title"),
      filters: [
        { name: t("materialPicker.filterName"), extensions: ["txt", "md", "docx"] },
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
      notice.value = t("notice.materialSaved");
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
  if (composerTextareaEl.value) composerTextareaEl.value.style.height = "auto";
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
async function copyMessage(content: string) {
  try {
    await navigator.clipboard.writeText(content);
  } catch (cause) {
    report(cause);
  }
}
async function deleteMessage(messageId: string) {
  if (!session.value || operationBusy.value) return;
  const target = session.value;
  target.messages = target.messages.filter((item) => item.id !== messageId);
  try {
    await invoke("save_session", { session: target });
  } catch (cause) {
    report(cause);
  }
}
function startEditMessage(id: string, content: string) {
  if (operationBusy.value) return;
  editingMessageId.value = id;
  editDraft.value = content;
}
function cancelEditMessage() {
  editingMessageId.value = "";
}
async function commitEditMessage() {
  if (!session.value || operationBusy.value) return;
  const id = editingMessageId.value;
  const content = editDraft.value.trim();
  editingMessageId.value = "";
  if (!id || !content) return;
  const target = session.value;
  const message = target.messages.find((item) => item.id === id);
  if (!message || message.content === content) return;
  message.content = content;
  try {
    await invoke("save_session", { session: target });
  } catch (cause) {
    report(cause);
  }
}
async function regenerateMessage(messageId: string) {
  if (!session.value || operationBusy.value) return;
  const target = session.value;
  const index = target.messages.findIndex((item) => item.id === messageId);
  if (index === -1 || target.messages[index]!.role !== "assistant") return;
  let userIndex = -1;
  for (let i = index - 1; i >= 0; i -= 1) {
    if (target.messages[i]!.role === "user") {
      userIndex = i;
      break;
    }
  }
  if (userIndex === -1) return;
  const content = target.messages[userIndex]!.content;
  const sessionId = target.id;
  target.messages = target.messages.slice(0, userIndex);
  streaming.value = true;
  error.value = "";
  try {
    await invoke("save_session", { session: target });
  } catch (cause) {
    streaming.value = false;
    report(cause);
    return;
  }
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
function handleContentClick(event: MouseEvent) {
  const button = (event.target as HTMLElement).closest<HTMLElement>(
    ".code-copy",
  );
  if (!button) return;
  const code = button.parentElement?.querySelector("pre")?.textContent ?? "";
  void copyMessage(code);
}
function handleComposerInput(event: Event) {
  const el = event.target as HTMLTextAreaElement;
  composerTextareaEl.value = el;
  el.style.height = "auto";
  el.style.height = `${Math.min(el.scrollHeight, 180)}px`;
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
      title: t("audioPicker.title"),
      filters: [
        {
          name: t("audioPicker.filterName"),
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
    notice.value = t("notice.settingsSaved");
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
    void persistLayoutSettings();
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
  void persistLayoutSettings();
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
  () => material.value?.id,
  () => {
    materialDraft.value = material.value?.content ?? "";
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
  () => data.value.settings.language,
  (language) => setLocale(language),
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
watch(operationBusy, (busy) => {
  if (!busy) return;
  closeContextMenu();
  cancelEditMessage();
  if (renamingId.value) cancelRename();
});
watch(renderedMessages, async () => {
  const panel = messagesRef.value;
  if (!panel) return;
  const distanceFromBottom =
    panel.scrollHeight - panel.scrollTop - panel.clientHeight;
  if (distanceFromBottom > 36) return;
  await nextTick();
  panel.scrollTop = panel.scrollHeight;
});
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
        >{{ error }} <button @click="error = ''">{{ t("common.close") }}</button></FluentNotice
      >
      <FluentNotice v-if="notice" tone="success" class="notice"
        >{{ notice }} <button @click="notice = ''">{{ t("common.close") }}</button></FluentNotice
      >
      <div v-if="loading" class="loading">
        <span class="spinner"></span> {{ t("loading") }}
      </div>
      <div v-else class="desktop-shell">
        <aside v-if="sidebarOpen" class="app-sidebar">
          <div class="sidebar-search">
            <FluentField
              v-model="searchQuery"
              :label="t('sidebar.search')"
              :placeholder="t('sidebar.searchPlaceholder')"
            />
          </div>
          <div class="sidebar-tabs">
            <FluentButton
              :class="{ active: sidebarMode === 'session' }"
              tone="subtle"
              @click="sidebarMode = 'session'"
              >{{ t("sidebar.tabs.sessions") }}</FluentButton
            ><FluentButton
              :class="{ active: sidebarMode === 'knowledge' }"
              tone="subtle"
              @click="sidebarMode = 'knowledge'"
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
                  :aria-label="t('sidebar.toggleWorkspaceAria')"
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
                  :aria-label="t('sidebar.deleteWorkspaceAria', { name: workspace.name })"
                  @click="askDelete('workspace', workspace.id, workspace.name)"
                  >{{ t("common.delete") }}</FluentButton
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
                    :aria-label="t('sidebar.deleteSessionAria', { name: item.title })"
                    @click="askDelete('session', item.id, item.title)"
                    >{{ t("common.delete") }}</FluentButton
                  >
                </div>
              </template>
            </section>
          </div>
          <div v-else class="sidebar-tree knowledge-tree">
            <div class="knowledge-head">
              <FluentSelect
                v-model="selectedWorkspaceId"
                :label="t('sidebar.knowledge.workspaceLabel')"
                :options="workspaceOptions"
                :disabled="operationBusy"
              />
              <span>{{ t("sidebar.knowledge.localMaterials") }}</span
              ><FluentButton
                tone="subtle"
                :disabled="!selectedWorkspaceId"
                @click="importMaterial"
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
                :aria-label="t('sidebar.deleteMaterialAria', { name: item.name })"
                @click="askDelete('material', item.id, item.name)"
                >{{ t("common.delete") }}</FluentButton
              >
            </div>
          </div>
          <footer class="sidebar-footer" v-if="sidebarMode === 'session'">
            <FluentButton
              tone="subtle"
              :disabled="!workspaces.length || operationBusy"
              @click="createQuickSession"
              >{{ t("sidebar.createSession") }}</FluentButton
            >
            <form @submit.prevent="createWorkspace">
              <FluentField
                v-model="workspaceName"
                :label="t('sidebar.knowledge.workspaceLabel')"
                :placeholder="t('sidebar.newWorkspacePlaceholder')"
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
              @click="importMaterial"
              >{{ t("sidebar.knowledge.import") }}</FluentButton
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
              :aria-label="t('header.toggleSidebar')"
              @click="toggleSidebar"
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
            <h1>{{ session?.title || t("appTitle") }}</h1>
            <div class="header-actions">
              <div class="header-switches" role="group" :aria-label="t('theme.switch')">
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
              <div class="header-switches" role="group" :aria-label="t('language.switch')">
                <button
                  type="button"
                  :class="{ active: data.settings.language === 'zh' }"
                  @click="setLanguage('zh')"
                >
                  {{ t("language.zh") }}
                </button>
                <button
                  type="button"
                  :class="{ active: data.settings.language === 'en' }"
                  @click="setLanguage('en')"
                >
                  {{ t("language.en") }}
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
                    : t("header.needsConnection")
                }}</span
              ><FluentButton tone="subtle" @click="settingsOpen = true"
                >{{ t("common.settings") }}</FluentButton
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
                  >{{ t("session.tabs.chat") }}</FluentButton
                ><FluentButton
                  :class="{ active: activeSessionTab === 'summary' }"
                  tone="subtle"
                  @click="activeSessionTab = 'summary'"
                  >{{ t("session.tabs.summary") }}</FluentButton
                >
              </div>
              <template v-if="activeSessionTab === 'chat'"
                ><div ref="messagesRef" class="messages" @click="handleContentClick">
                  <article
                    v-for="message in renderedMessages"
                    :key="message.id"
                    :class="['message', message.role]"
                  >
                    <label>{{
                      message.role === "user" ? t("session.you") : t("session.assistant")
                    }}</label>
                    <div class="message-actions">
                      <button
                        type="button"
                        :disabled="operationBusy"
                        @click="copyMessage(message.content)"
                      >
                        {{ t("session.actions.copy") }}
                      </button>
                      <button
                        v-if="message.role === 'user'"
                        type="button"
                        :disabled="operationBusy"
                        @click="startEditMessage(message.id, message.content)"
                      >
                        {{ t("session.actions.edit") }}
                      </button>
                      <button
                        v-if="message.role === 'assistant'"
                        type="button"
                        :disabled="operationBusy"
                        @click="regenerateMessage(message.id)"
                      >
                        {{ t("session.actions.regenerate") }}
                      </button>
                      <button
                        type="button"
                        :disabled="operationBusy"
                        @click="deleteMessage(message.id)"
                      >
                        {{ t("session.actions.delete") }}
                      </button>
                    </div>
                    <div
                      v-if="editingMessageId === message.id"
                      class="message-edit"
                    >
                      <FluentTextArea v-model="editDraft" :label="t('session.editMessageLabel')" />
                      <div class="message-edit-actions">
                        <FluentButton
                          tone="subtle"
                          :disabled="operationBusy"
                          @click="cancelEditMessage"
                          >{{ t("common.cancel") }}</FluentButton
                        ><FluentButton
                          tone="primary"
                          :disabled="operationBusy"
                          @click="commitEditMessage"
                          >{{ t("common.save") }}</FluentButton
                        >
                      </div>
                    </div>
                    <div v-else class="message-body" v-html="message.html"></div>
                  </article>
                  <div v-if="!renderedMessages.length" class="empty center">
                    {{ t("session.emptyChat") }}
                  </div>
                </div>
                <form class="composer" @submit.prevent="send">
                  <FluentTextArea
                    v-model="draft"
                    :label="t('session.inputLabel')"
                    :disabled="operationBusy"
                    :placeholder="t('session.inputPlaceholder')"
                    @keydown.enter.exact.prevent="handleComposerEnter"
                    @input="handleComposerInput"
                  /><FluentButton v-if="streaming" tone="danger" @click="cancel"
                    >{{ t("session.stop") }}</FluentButton
                  ><FluentButton
                    v-else
                    tone="primary"
                    type="submit"
                    :disabled="
                      !draft.trim() || !currentProvider?.hasKey || operationBusy
                    "
                    >{{ t("session.send") }}</FluentButton
                  >
                </form></template
              >
              <section v-else class="summary-page">
                <header>
                  <div>
                    <h2>{{ t("summary.title") }}</h2>
                    <p>{{ t("summary.description") }}</p>
                  </div>
                  <FluentButton
                    :tone="summaryLoading ? 'danger' : 'secondary'"
                    :disabled="operationBusy && !summaryLoading"
                    @click="summaryLoading ? cancel() : summarize()"
                    >{{
                      summaryLoading ? t("summary.stop") : t("summary.generate")
                    }}</FluentButton
                  >
                </header>
                <p v-if="summaryUpdatedLabel" class="summary-meta">
                  {{ t("summary.updatedAt", { time: summaryUpdatedLabel }) }}
                </p>
                <div v-if="summaryLoading && !summaryStream" class="empty">
                  <span class="spinner"></span> {{ t("summary.generating") }}
                </div>
                <div
                  v-else-if="session.summary || summaryStream"
                  class="summary-content"
                  v-html="md.render(summaryStream || session.summary || '')"
                  @click="handleContentClick"
                ></div>
                <div v-else class="empty">
                  {{ t("summary.empty") }}
                </div>
              </section>
            </section>
            <div
              class="resize-handle"
              role="separator"
              tabindex="0"
              :aria-label="t('transcript.resizeAria')"
              aria-orientation="vertical"
              :aria-valuenow="Math.round(mainPanelRatio * 100)"
              @pointerdown="beginResize"
              @keydown="resizeWithKeyboard"
            ></div>
            <aside class="transcript-panel">
              <header>
                <div>
                  <h2>{{ t("transcript.title") }}</h2>
                  <p>
                    {{
                      stt.ready ? t("transcript.readyHint") : t("transcript.notReadyHint")
                    }}
                  </p>
                </div>
                <FluentButton
                  tone="subtle"
                  :busy="transcriptionLoading"
                  :disabled="!session || operationBusy"
                  @click="uploadAudio"
                  >{{ t("transcript.import") }}</FluentButton
                >
              </header>
              <div ref="transcriptScrollRef" class="transcript-content">
                <p v-if="session?.transcription">{{ session.transcription }}</p>
                <div v-else class="empty transcript-empty">
                  <svg
                    width="32"
                    height="32"
                    viewBox="0 0 24 24"
                    fill="none"
                    aria-hidden="true"
                  >
                    <path
                      d="M12 15a3 3 0 0 0 3-3V6a3 3 0 1 0-6 0v6a3 3 0 0 0 3 3Z"
                      stroke="currentColor"
                      stroke-width="1.4"
                    /><path
                      d="M5 11v1a7 7 0 0 0 14 0v-1M12 19v3"
                      stroke="currentColor"
                      stroke-width="1.4"
                      stroke-linecap="round"
                    />
                  </svg>
                  <p>{{ t("transcript.empty") }}</p>
                </div>
              </div>
              <footer class="recording-bar">
                <span class="recording-state">{{
                  recordingStarting
                    ? t("transcript.recording.loadingModel")
                    : recording
                      ? t("transcript.recording.active")
                      : transcriptionLoading
                        ? t("transcript.recording.transcribing")
                        : t("transcript.recording.idle")
                }}</span
                ><FluentButton
                  v-if="transcriptionLoading && !recordingSessionId"
                  tone="danger"
                  @click="cancel"
                  >{{ t("transcript.recording.cancel") }}</FluentButton
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
                  >{{ recording ? t("transcript.recording.stop") : t("transcript.recording.start") }}</FluentButton
                >
              </footer>
            </aside>
          </template>
          <section v-else class="main-workspace">
            <div v-if="material" class="material-editor-panel">
              <h2>{{ material.name }}</h2>
              <FluentTextArea
                v-model="materialDraft"
                :label="t('sidebar.knowledge.materialContent')"
                class="material-editor"
              />
              <div class="material-actions">
                <FluentButton tone="secondary" @click="saveMaterial"
                  >{{ t("common.save") }}</FluentButton
                ><FluentButton
                  tone="danger"
                  @click="askDelete('material', material.id, material.name)"
                  >{{ t("common.delete") }}</FluentButton
                >
              </div>
            </div>
            <div v-else class="workbench-empty">
              <h2>{{ t("workbenchEmpty.title") }}</h2>
              <p>
                {{ t("workbenchEmpty.body") }}
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
          {{ t("sidebar.createSession") }}
        </button>
        <button type="button" @click="contextMenuRename">{{ t("sidebar.menu.rename") }}</button>
        <button type="button" class="danger" @click="contextMenuDelete">
          {{ t("sidebar.menu.delete") }}
        </button>
      </div>
      <FluentDialog v-model:open="sessionCreateOpen" :label="t('sessionCreate.title')">
        <template #title><h2>{{ t("sessionCreate.title") }}</h2></template>
        <template #default>
          <p class="dialog-description">
            {{ t("sessionCreate.description") }}
          </p>
          <FluentSelect
            v-model="sessionCreateWorkspaceId"
            :label="t('sidebar.knowledge.workspaceLabel')"
            :options="workspaceOptions"
          />
          <FluentField
            v-model="sessionTitle"
            :label="t('sessionCreate.label')"
            :placeholder="t('sessionCreate.placeholder')"
          />
        </template>
        <template #footer
          ><FluentButton tone="subtle" @click="sessionCreateOpen = false"
            >{{ t("common.cancel") }}</FluentButton
          ><FluentButton
            tone="primary"
            :disabled="!sessionTitle.trim() || !sessionCreateWorkspaceId"
            @click="
              createSession();
              sessionCreateOpen = false;
            "
            >{{ t("sessionCreate.submit") }}</FluentButton
          ></template
        >
      </FluentDialog>
      <FluentDialog v-model:open="settingsOpen" :label="t('settingsDialog.title')"
        ><template #title><h2>{{ t("settingsDialog.title") }}</h2></template>
        <template #default
          ><div class="settings">
            <ModelConnections
              :theme="data.settings.theme"
              :refresh-workbench="refreshModelSettings"
            />
            <section class="stt-status">
              <strong>{{ t("stt.title") }}</strong>
              <p>
                {{
                  stt.ready
                    ? t("stt.readyDetail", { name: stt.modelName })
                    : t("stt.notReadyDetail")
                }}
              </p>
              <FluentProgressBar
                v-if="sttDownloading"
                :label="t('stt.downloadProgress')"
                :indeterminate="!sttProgress.total"
                :value="sttProgress.downloaded"
                :max="sttProgress.total || 1"
              /><FluentButton
                v-if="sttDownloading"
                tone="danger"
                @click="cancelStt"
                >{{ t("stt.cancelDownload") }}</FluentButton
              ><FluentButton
                v-else
                tone="secondary"
                :disabled="stt.ready"
                @click="downloadStt"
                >{{
                  stt.ready ? t("stt.ready") : t("stt.download")
                }}</FluentButton
              >
            </section></div></template
        ><template #footer
          ><FluentButton tone="subtle" @click="settingsOpen = false"
            >{{ t("common.close") }}</FluentButton
          ><FluentButton tone="primary" @click="saveSettings"
            >{{ t("settingsDialog.save") }}</FluentButton
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
            >{{ t("common.cancel") }}</FluentButton
          ><FluentButton
            tone="danger"
            :disabled="operationBusy"
            @click="confirmDelete"
            >{{ deleteCopy.confirmLabel }}</FluentButton
          ></template
        ></FluentDialog
      >
    </main>
  </FluentTheme>
</template>
