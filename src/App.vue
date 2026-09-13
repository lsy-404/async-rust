<script setup lang="ts">
import {
  computed,
  nextTick,
  onMounted,
  onUnmounted,
  reactive,
  ref,
  watch,
} from "vue";
import MarkdownIt from "markdown-it";
import texmath from "markdown-it-texmath";
import katex from "katex";
import { invoke, Channel } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import ModelConnections from "./components/ModelConnections.vue";
import ActivityBar from "./components/ActivityBar.vue";
import ExplorerView from "./components/ExplorerView.vue";
import SearchView from "./components/SearchView.vue";
import WorkbenchChat from "./components/WorkbenchChat.vue";
import TranscriptPanel from "./components/TranscriptPanel.vue";
import { i18n, setLocale } from "./locales";
import { splitTranscriptSentences } from "./transcript-sentences";
import {
  ancestorsOf,
  flattenVisible,
  nextFocusAfterDelete,
  resolveCreateTarget,
  subtreeIds,
} from "./explorer/tree";
import {
  FluentButton,
  FluentDialog,
  FluentNotice,
  FluentProgressBar,
  FluentTextArea,
  FluentTheme,
} from "@platform-kit/fluent/vue";
import type {
  AppData,
  CaptureMode,
  Node,
  NotesStatus,
  NotesStatusEvent,
  RecordingSource,
  SearchResults,
  Session,
  Settings,
  StreamEvent,
  SttStatus,
  SystemAudioCapability,
  RecordingEvent,
  Theme,
  ToolCall,
  TranslationMode,
  TranslationStatusEvent,
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
// markdown-it runs with html:false, so a literal <maybe> the model emits arrives
// HTML-escaped; an unpaired tag (mid-stream) is stripped rather than shown raw.
function highlightUncertain(html: string): string {
  const span = '<span class="uncertain-span">$1</span>';
  return html
    .replace(/&lt;maybe&gt;([\s\S]*?)&lt;\/maybe&gt;/g, span)
    .replace(/<maybe>([\s\S]*?)<\/maybe>/g, span)
    .replace(/&lt;\/?maybe&gt;/g, "")
    .replace(/<\/?maybe>/g, "");
}
const blankSettings: Settings = {
  providerId: "openai",
  model: "",
  theme: "system",
  language: "zh",
};
const data = ref<AppData>({
  nodes: [],
  sessions: [],
  materials: [],
  settings: { ...blankSettings },
  providers: [],
});
// The single node currently open in the workbench; empty means the empty
// state. Selecting a session or material in the explorer sets this.
const openNodeId = ref("");
const draft = ref("");
const notice = ref("");
const error = ref("");
const loading = ref(true);
const streaming = ref(false);
const summaryLoading = ref(false);
const summaryStream = ref("");
// Pushed by the backend's background notes job, keyed by session id; absent
// means no background round has reported in for that session yet.
const notesStatusBySession = reactive<Record<string, NotesStatus>>({});
let unlistenNotesStatus: UnlistenFn | undefined;
// Translation cache/in-flight tracking is global (not per session), keyed by
// `${targetLanguage} ${sourceSentenceText}` so a lookup depends only on
// a sentence's own content plus the language, never its position - a growing
// or re-segmenting transcript can never make a translation land on the wrong
// sentence.
const TRANSLATION_KEY_SEP = " ";
function translationKey(targetLanguage: string, sentence: string): string {
  return `${targetLanguage}${TRANSLATION_KEY_SEP}${sentence}`;
}
const sentenceTranslations = reactive<Record<string, string>>({});
const translatingKeys = reactive<Set<string>>(new Set());
// Set from a `translation-status` event's `error`, keyed by session+target
// language so switching either never shows another pair's stale failure;
// cleared by the next error-free event for that same pair. Mirrors
// notesStatusBySession's "error" state for the identical failure mode: a
// batch keeps failing and retrying in the background with nothing in the UI
// to show it.
const translationErrorBySession = reactive<Record<string, string>>({});
function translationStatusKey(sessionId: string, targetLanguage: string): string {
  return `${sessionId}${TRANSLATION_KEY_SEP}${targetLanguage}`;
}
let unlistenTranslationStatus: UnlistenFn | undefined;
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
const materialDraft = ref("");
const recording = ref(false);
const recordingStarting = ref(false);
const recordingSessionId = ref("");
let recordingGeneration = 0;
const recordingLevel = ref(0);
const captureMode = ref<CaptureMode>("realtime");
const systemAudioCapability = ref<SystemAudioCapability>({
  available: false,
  reason: null,
});
const transcriptionLanguage = ref("");
const importedAudioUrl = ref<string>();
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
const workbenchChatRef = ref<InstanceType<typeof WorkbenchChat>>();
const transcriptPanelRef = ref<InstanceType<typeof TranscriptPanel>>();
const composerTextareaEl = ref<HTMLTextAreaElement>();
const editingMessageId = ref("");
const editDraft = ref("");
const expandedFolders = ref(new Set<string>());
let expandedFoldersPersistTimer: ReturnType<typeof setTimeout> | undefined;
const focusedNodeId = ref<string | null>(null);
const contextMenu = ref<{ x: number; y: number; nodeId: string | null }>();
const inlineCreate = ref<{ parentId: string | null; kind: "session" | "folder" }>();
const createDraft = ref("");
const renamingId = ref("");
const renameDraft = ref("");
const deleteTarget = ref<{ nodeId: string }>();
const dragNodeId = ref<string>();
const dropParent = ref<string | null>();
const activeView = ref<"explorer" | "search">("explorer");
const explorerViewRef = ref<InstanceType<typeof ExplorerView>>();
const searchViewRef = ref<InstanceType<typeof SearchView>>();
const searchQuery = ref("");
const searchResults = ref<SearchResults>();
let searchDebounceTimer: ReturnType<typeof setTimeout> | undefined;
let searchToken = 0;
const themeChoices = computed<{ value: Theme; label: string }[]>(() => [
  { value: "system", label: t("theme.system") },
  { value: "light", label: t("theme.light") },
  { value: "dark", label: t("theme.dark") },
]);

function nodeName(id: string): string {
  return data.value.nodes.find((item) => item.id === id)?.name ?? "";
}
const activeNode = computed(() =>
  data.value.nodes.find((item) => item.id === openNodeId.value),
);
const focusedNode = computed(
  () => data.value.nodes.find((item) => item.id === focusedNodeId.value) ?? null,
);
const session = computed(() =>
  activeNode.value?.kind === "session"
    ? data.value.sessions.find((item) => item.id === openNodeId.value)
    : undefined,
);
const material = computed(() =>
  activeNode.value?.kind === "material"
    ? data.value.materials.find((item) => item.id === openNodeId.value)
    : undefined,
);
const currentProvider = computed(() =>
  data.value.providers.find(
    (item) => item.id === data.value.settings.providerId,
  ),
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
      html: highlightUncertain(md.render(message.content)),
    })) ?? [],
);
const summaryHtml = computed(() =>
  md.render(summaryStream.value || session.value?.summary || ""),
);
const notesEnabled = computed(() => session.value?.notesEnabled ?? true);
const providerConnected = computed(() =>
  Boolean(currentProvider.value?.hasKey && data.value.settings.model),
);
const notesStatus = computed<NotesStatus>(() => {
  if (!notesEnabled.value) return "idle";
  if (!providerConnected.value) return "needs-provider";
  return notesStatusBySession[session.value?.id ?? ""] ?? "idle";
});
const translationEnabled = computed(() => session.value?.translationEnabled ?? false);
const translationMode = computed<TranslationMode>(
  () => session.value?.translationMode ?? "side-by-side",
);
// Defaults to the app's own UI language until the user picks a specific
// target language for this session.
const translationTargetLanguage = computed(
  () => session.value?.translationTargetLanguage || data.value.settings.language,
);
const currentSentenceTranslations = computed<Record<string, string>>(() => {
  const prefix = `${translationTargetLanguage.value}${TRANSLATION_KEY_SEP}`;
  const out: Record<string, string> = {};
  for (const key of Object.keys(sentenceTranslations)) {
    if (key.startsWith(prefix)) out[key.slice(prefix.length)] = sentenceTranslations[key]!;
  }
  return out;
});
const currentTranslatingSentences = computed<Set<string>>(() => {
  const prefix = `${translationTargetLanguage.value}${TRANSLATION_KEY_SEP}`;
  const out = new Set<string>();
  for (const key of translatingKeys) {
    if (key.startsWith(prefix)) out.add(key.slice(prefix.length));
  }
  return out;
});
const translationError = computed<string | undefined>(() => {
  if (!session.value) return undefined;
  return translationErrorBySession[
    translationStatusKey(session.value.id, translationTargetLanguage.value)
  ];
});
async function setTranslationSettings(
  partial: Partial<{
    enabled: boolean;
    targetLanguage: string | null;
    mode: TranslationMode;
  }>,
) {
  if (!session.value) return;
  const target = session.value;
  const previous = {
    enabled: target.translationEnabled ?? false,
    targetLanguage: target.translationTargetLanguage ?? null,
    mode: target.translationMode ?? "side-by-side",
  };
  const next = { ...previous, ...partial };
  target.translationEnabled = next.enabled;
  target.translationTargetLanguage = next.targetLanguage;
  target.translationMode = next.mode;
  try {
    await invoke("set_translation_settings", {
      id: target.id,
      enabled: next.enabled,
      targetLanguage: next.targetLanguage,
      mode: next.mode,
    });
    if (next.enabled) syncTranslationQueue();
  } catch (cause) {
    target.translationEnabled = previous.enabled;
    target.translationTargetLanguage = previous.targetLanguage;
    target.translationMode = previous.mode;
    report(cause);
  }
}
// Submits every finalized sentence that isn't already cached or already
// in flight for the current session's target language. Never sends a
// per-sentence request: the backend itself coalesces whatever this adds up
// into pending into one short-debounced batch, so calling this often (e.g.
// on every transcript tick) is cheap and safe.
function syncTranslationQueue() {
  const target = session.value;
  if (!target || !translationEnabled.value || !providerConnected.value) return;
  const allSentences = splitTranscriptSentences(target.transcription ?? "");
  if (!allSentences.length) return;
  // While this session is the one actively recording, the last split
  // "sentence" may just be a still-growing fragment without terminal
  // punctuation yet - translating it now would be thrown away as soon as
  // more words arrive, so it's left for the next tick (or for the moment
  // recording stops, when it becomes final).
  const isLiveForThisSession = recording.value && recordingSessionId.value === target.id;
  const finalized = isLiveForThisSession ? allSentences.slice(0, -1) : allSentences;
  const lang = translationTargetLanguage.value;
  const need = finalized.filter((sentence) => {
    const key = translationKey(lang, sentence);
    return !(key in sentenceTranslations) && !translatingKeys.has(key);
  });
  if (!need.length) return;
  for (const sentence of need) translatingKeys.add(translationKey(lang, sentence));
  const sessionId = target.id;
  invoke<Record<string, string>>("queue_sentence_translations", {
    sessionId,
    targetLanguage: lang,
    sentences: need,
  })
    .then((hits) => {
      for (const [sentence, translation] of Object.entries(hits)) {
        sentenceTranslations[translationKey(lang, sentence)] = translation;
        translatingKeys.delete(translationKey(lang, sentence));
      }
    })
    .catch((cause) => {
      for (const sentence of need) translatingKeys.delete(translationKey(lang, sentence));
      report(cause);
    });
}
async function toggleNotesEnabled(enabled: boolean) {
  if (!session.value) return;
  const target = session.value;
  target.notesEnabled = enabled;
  try {
    await invoke("set_notes_enabled", { id: target.id, enabled });
  } catch (cause) {
    target.notesEnabled = !enabled;
    report(cause);
  }
}
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
      const folderIds = new Set(
        data.value.nodes
          .filter((item) => item.kind === "folder")
          .map((item) => item.id),
      );
      expandedFolders.value = new Set(
        (data.value.settings.explorerExpanded ?? []).filter((id) =>
          folderIds.has(id),
        ),
      );
    }
    if (
      openNodeId.value &&
      !data.value.nodes.some((item) => item.id === openNodeId.value)
    )
      openNodeId.value = "";
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
// Opens a session or material in the workbench. Refused while an operation
// is busy, unless force is set (used by quick transcription, which must open
// the session it just started recording into).
function openNode(id: string, options: { force?: boolean } = {}) {
  if (operationBusy.value && !options.force && id !== openNodeId.value) return;
  openNodeId.value = id;
}
// Debounced 500ms, shared by every explorerExpanded mutation (manual toggle
// and drag-hover auto-expand alike).
function schedulePersistExpandedFolders() {
  if (expandedFoldersPersistTimer) clearTimeout(expandedFoldersPersistTimer);
  expandedFoldersPersistTimer = setTimeout(() => {
    data.value.settings.explorerExpanded = [...expandedFolders.value];
    void invoke("save_settings", { settings: data.value.settings }).catch(report);
  }, 500);
}
function toggleFolder(id: string) {
  if (expandedFolders.value.has(id)) expandedFolders.value.delete(id);
  else expandedFolders.value.add(id);
  schedulePersistExpandedFolders();
}
function focusNode(id: string | null) {
  focusedNodeId.value = id;
}
function openContextMenu(payload: { x: number; y: number; nodeId: string | null }) {
  if (operationBusy.value) return;
  focusedNodeId.value = payload.nodeId;
  contextMenu.value = payload;
}
function closeContextMenu() {
  contextMenu.value = undefined;
}
function startCreate(kind: "session" | "folder") {
  if (operationBusy.value) return;
  const parentId = resolveCreateTarget(focusedNode.value);
  if (parentId) expandedFolders.value.add(parentId);
  contextMenu.value = undefined;
  createDraft.value = "";
  inlineCreate.value = { parentId, kind };
}
function cancelCreate() {
  inlineCreate.value = undefined;
  createDraft.value = "";
}
async function commitCreate() {
  const create = inlineCreate.value;
  if (!create || operationBusy.value) return;
  const name = createDraft.value.trim();
  inlineCreate.value = undefined;
  createDraft.value = "";
  if (!name) return;
  try {
    const command = create.kind === "session" ? "create_session" : "create_folder";
    const node = await invoke<Node>(command, { parentId: create.parentId, name });
    await refresh();
    focusedNodeId.value = node.id;
    if (create.kind === "session") openNode(node.id);
  } catch (cause) {
    report(cause);
  }
}
async function startImportMaterial() {
  if (operationBusy.value) return;
  const parentId = resolveCreateTarget(focusedNode.value);
  contextMenu.value = undefined;
  try {
    const path = await open({
      multiple: false,
      title: t("explorer.importDialogTitle"),
      filters: [
        {
          name: t("explorer.importFilterName"),
          extensions: ["txt", "md", "markdown", "docx"],
        },
      ],
    });
    if (typeof path !== "string") return;
    const node = await invoke<Node>("import_material", { parentId, path });
    await refresh();
    if (parentId) expandedFolders.value.add(parentId);
    focusedNodeId.value = node.id;
  } catch (cause) {
    report(cause);
  }
}
function startRename(id: string) {
  if (operationBusy.value) return;
  const target = data.value.nodes.find((item) => item.id === id);
  if (!target) return;
  contextMenu.value = undefined;
  renamingId.value = id;
  renameDraft.value = target.name;
}
function cancelRename() {
  renamingId.value = "";
  renameDraft.value = "";
}
async function commitRename() {
  const id = renamingId.value;
  if (!id) return;
  const name = renameDraft.value.trim();
  const target = data.value.nodes.find((item) => item.id === id);
  renamingId.value = "";
  renameDraft.value = "";
  if (!name || !target || name === target.name || operationBusy.value) return;
  try {
    await invoke("rename_node", { id, name });
    await refresh();
    focusedNodeId.value = id;
  } catch (cause) {
    report(cause);
  }
}
function openDeleteDialog(id: string) {
  if (operationBusy.value) return;
  contextMenu.value = undefined;
  deleteTarget.value = { nodeId: id };
}
function cancelDelete() {
  deleteTarget.value = undefined;
}
async function confirmDelete() {
  const target = deleteTarget.value;
  if (!target || operationBusy.value) return;
  const id = target.nodeId;
  const flat = flattenVisible(
    data.value.nodes,
    expandedFolders.value,
    data.value.settings.language,
  );
  const nextFocus = nextFocusAfterDelete(flat, id);
  const subtree = subtreeIds(data.value.nodes, id);
  deleteTarget.value = undefined;
  try {
    await invoke("delete_node", { id });
    await refresh();
    if (openNodeId.value && subtree.includes(openNodeId.value)) openNodeId.value = "";
    focusedNodeId.value = nextFocus;
    let expandedChanged = false;
    for (const removedId of subtree) {
      if (expandedFolders.value.delete(removedId)) expandedChanged = true;
    }
    if (expandedChanged) {
      data.value.settings.explorerExpanded = [...expandedFolders.value];
      await invoke("save_settings", { settings: data.value.settings }).catch(report);
    }
  } catch (cause) {
    report(cause);
  }
}
function handleDragStart(id: string) {
  dragNodeId.value = id;
  dropParent.value = undefined;
}
function handleDragOver(parentId: string | null) {
  dropParent.value = parentId;
}
function handleDragLeave() {
  dropParent.value = undefined;
}
function handleDragEnd() {
  dragNodeId.value = undefined;
  dropParent.value = undefined;
}
// A folder hovered 700ms during a drag auto-expands, same as a manual
// toggle, but never collapses one already open.
function autoExpandFolder(id: string) {
  if (expandedFolders.value.has(id)) return;
  expandedFolders.value.add(id);
  schedulePersistExpandedFolders();
}
async function handleNodeDrop(payload: { id: string; parentId: string | null }) {
  if (operationBusy.value) return;
  try {
    await invoke("move_node", { id: payload.id, parentId: payload.parentId });
    await refresh();
    if (payload.parentId) {
      expandedFolders.value.add(payload.parentId);
      for (const ancestor of ancestorsOf(data.value.nodes, payload.parentId)) {
        expandedFolders.value.add(ancestor.id);
      }
    }
    focusedNodeId.value = payload.id;
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
// Opens the given view, opening the side panel if it was closed (used by the
// keyboard shortcuts, which always open rather than toggle).
function openView(view: "explorer" | "search") {
  activeView.value = view;
  if (!sidebarOpen.value) toggleSidebar();
}
// VS Code behaviour: clicking the inactive icon switches to it (opening the
// panel if needed); clicking the already-active one toggles the panel.
function selectActivityView(view: "explorer" | "search") {
  if (activeView.value === view) {
    toggleSidebar();
    return;
  }
  openView(view);
}
async function focusExplorerRow() {
  await nextTick();
  explorerViewRef.value?.focusRow();
}
async function focusSearchInput() {
  await nextTick();
  searchViewRef.value?.focusQuery();
}
function handleWindowKeydown(event: KeyboardEvent) {
  if (event.isComposing || event.keyCode === 229) return;
  if (!event.ctrlKey && !event.metaKey) return;
  const key = event.key.toLowerCase();
  if (key === "b") {
    event.preventDefault();
    toggleSidebar();
    return;
  }
  if (!event.shiftKey) return;
  if (key === "e") {
    event.preventDefault();
    openView("explorer");
    void focusExplorerRow();
    return;
  }
  if (key === "f") {
    event.preventDefault();
    openView("search");
    void focusSearchInput();
  }
}
function runSearch(query: string) {
  const trimmed = query.trim();
  const token = ++searchToken;
  if (!trimmed) {
    searchResults.value = undefined;
    return;
  }
  invoke<SearchResults>("search_library", { query: trimmed })
    .then((results) => {
      if (token === searchToken) searchResults.value = results;
    })
    .catch((cause) => {
      if (token === searchToken) report(cause);
    });
}
// Opens a search hit's node and reveals it in the explorer tree (expanding
// every ancestor folder and focusing the row), without switching away from
// the search view - matching VS Code's own search-result behaviour.
function openFromSearch(id: string) {
  openNode(id);
  for (const ancestor of ancestorsOf(data.value.nodes, id)) {
    expandedFolders.value.add(ancestor.id);
  }
  focusedNodeId.value = id;
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
// Shared by send/regenerate/edit: appends the user turn and a streaming
// placeholder, then runs chat. A generation failure renders inline on that
// turn (matching the reference) instead of dropping it behind a global notice.
async function streamAssistantReply(target: Session, content: string) {
  const sessionId = target.id;
  streaming.value = true;
  error.value = "";
  const pending = reactive({
    id: `streaming-${sessionId}`,
    role: "assistant" as const,
    content: "",
    toolCalls: [] as ToolCall[],
  });
  target.messages.push(
    { id: crypto.randomUUID(), role: "user", content },
    pending,
  );
  const channel = new Channel<StreamEvent>();
  channel.onmessage = (event) => {
    if (event.type === "delta") {
      pending.content += event.text;
      return;
    }
    if (event.type === "tool" && event.toolCallId) {
      const existing = pending.toolCalls.find(
        (call) => call.id === event.toolCallId,
      );
      if (existing) {
        existing.status = event.toolStatus ?? existing.status;
        if (event.toolArguments !== undefined)
          existing.arguments = event.toolArguments;
        if (event.toolResult !== undefined) existing.result = event.toolResult;
      } else {
        pending.toolCalls.push({
          id: event.toolCallId,
          name: event.toolName ?? "",
          status: event.toolStatus ?? "requested",
          arguments: event.toolArguments,
          result: event.toolResult,
        });
      }
    }
  };
  try {
    await streamCommand(invoke, "chat", sessionId, channel, content);
    await refresh();
  } catch (cause) {
    pending.content = `${t("session.errorPrefix")} ${String(cause)}`;
  } finally {
    streaming.value = false;
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
  const content = draft.value.trim();
  draft.value = "";
  if (composerTextareaEl.value) composerTextareaEl.value.style.height = "auto";
  await streamAssistantReply(target, content);
}
function askUncertain(text: string) {
  if (!session.value || !text.trim() || operationBusy.value) return;
  draft.value = t("session.uncertain.prompt", { text: text.trim() });
  void send();
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
    await invoke("save_messages", {
      sessionId: target.id,
      messages: target.messages,
    });
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
  const index = target.messages.findIndex((item) => item.id === id);
  if (index === -1 || target.messages[index]!.content === content) return;
  // Truncate from the edited turn on, same as regenerate, so the reply below
  // it can never end up answering a question that no longer exists.
  target.messages = target.messages.slice(0, index);
  streaming.value = true;
  error.value = "";
  try {
    await invoke("save_messages", {
      sessionId: target.id,
      messages: target.messages,
    });
  } catch (cause) {
    streaming.value = false;
    report(cause);
    return;
  }
  await streamAssistantReply(target, content);
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
  target.messages = target.messages.slice(0, userIndex);
  streaming.value = true;
  error.value = "";
  try {
    await invoke("save_messages", {
      sessionId: target.id,
      messages: target.messages,
    });
  } catch (cause) {
    streaming.value = false;
    report(cause);
    return;
  }
  await streamAssistantReply(target, content);
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
function guessAudioMime(path: string): string {
  const extension = path.split(".").pop()?.toLowerCase() ?? "";
  const byExtension: Record<string, string> = {
    wav: "audio/wav",
    mp3: "audio/mpeg",
    flac: "audio/flac",
    ogg: "audio/ogg",
    m4a: "audio/mp4",
    aac: "audio/aac",
  };
  return byExtension[extension] ?? "audio/mpeg";
}
function revokeImportedAudio() {
  if (importedAudioUrl.value) URL.revokeObjectURL(importedAudioUrl.value);
  importedAudioUrl.value = undefined;
}
async function loadImportedAudio(path: string) {
  revokeImportedAudio();
  try {
    const bytes = await invoke<ArrayBuffer>("read_audio_file", { path });
    importedAudioUrl.value = URL.createObjectURL(
      new Blob([bytes], { type: guessAudioMime(path) }),
    );
  } catch (cause) {
    report(cause);
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
      await loadImportedAudio(path);
      await invoke("transcribe_audio", {
        sessionId,
        path,
        language: transcriptionLanguage.value || null,
      });
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
      recordingLevel.value = 0;
    }
    return;
  }
  revokeImportedAudio();
  const sessionId = session.value.id;
  recordingStarting.value = true;
  recordingLevel.value = 0;
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
      recordingLevel.value = 0;
      void invoke("cancel_recording", { sessionId }).catch(report);
      return;
    }
    if (event.type === "level") {
      recordingLevel.value = event.level ?? 0;
      return;
    }
    const target = data.value.sessions.find((item) => item.id === sessionId);
    if (target) target.transcription = event.text;
  };
  const source: RecordingSource =
    captureMode.value === "system" ? "systemAudio" : "microphone";
  try {
    await invoke("start_recording", {
      sessionId,
      onEvent,
      source,
      language: transcriptionLanguage.value || null,
    });
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
function handleComposerEnter() {
  // IME composition safety already gated in WorkbenchChat before this fires.
  void send();
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
  () => session.value?.transcription,
  async () => {
    const panel = transcriptPanelRef.value?.transcriptScrollRef;
    if (!panel) return;
    const distanceFromBottom =
      panel.scrollHeight - panel.scrollTop - panel.clientHeight;
    if (distanceFromBottom > 36) return;
    await nextTick();
    panel.scrollTop = panel.scrollHeight;
  },
);
watch(openNodeId, () => {
  revokeImportedAudio();
});
// Covers every trigger for (re)submitting sentences for translation: the
// transcript growing (live recording, an upload, or stop_recording's final
// refresh), the toggle turning on, the target language changing (a language
// switch never needs active invalidation - the cache is already partitioned
// by language, so this just requests whatever isn't cached under the new
// one), and switching to a different session entirely.
watch(() => session.value?.transcription, syncTranslationQueue);
watch(translationEnabled, (enabled) => {
  if (enabled) syncTranslationQueue();
});
watch(translationTargetLanguage, syncTranslationQueue);
watch(openNodeId, syncTranslationQueue);
watch(operationBusy, (busy) => {
  if (!busy) return;
  cancelEditMessage();
  closeContextMenu();
  cancelRename();
  cancelCreate();
  handleDragEnd();
});
watch(searchQuery, (query) => {
  if (searchDebounceTimer) clearTimeout(searchDebounceTimer);
  searchDebounceTimer = setTimeout(() => runSearch(query), 200);
});
watch(renderedMessages, async () => {
  const panel = workbenchChatRef.value?.messagesRef;
  if (!panel) return;
  const distanceFromBottom =
    panel.scrollHeight - panel.scrollTop - panel.clientHeight;
  if (distanceFromBottom > 36) return;
  await nextTick();
  panel.scrollTop = panel.scrollHeight;
});
onMounted(() => {
  void refresh();
  // Capture hardware/OS support doesn't change while the app runs, so this is
  // fetched once rather than on every refresh().
  void invoke<SystemAudioCapability>("get_system_audio_capability")
    .then((capability) => {
      if (capability) systemAudioCapability.value = capability;
    })
    .catch(() => {});
  window.addEventListener("keydown", handleWindowKeydown);
  listen<NotesStatusEvent>("notes-status", (event) => {
    const payload = event.payload;
    notesStatusBySession[payload.sessionId] = payload.status;
    if (payload.status !== "idle" && payload.status !== "error") return;
    const target = data.value.sessions.find(
      (item) => item.id === payload.sessionId,
    );
    if (target && payload.summary !== undefined) {
      target.summary = payload.summary ?? undefined;
      target.summaryUpdatedAt = payload.summaryUpdatedAt ?? undefined;
    }
  })
    .then((unlisten) => {
      unlistenNotesStatus = unlisten;
    })
    .catch(() => {});
  listen<TranslationStatusEvent>("translation-status", (event) => {
    const payload = event.payload;
    for (const sentence of payload.sentences) {
      translatingKeys.delete(translationKey(payload.targetLanguage, sentence));
    }
    for (const [sentence, translation] of Object.entries(payload.translations)) {
      sentenceTranslations[translationKey(payload.targetLanguage, sentence)] = translation;
    }
    const statusKey = translationStatusKey(payload.sessionId, payload.targetLanguage);
    if (payload.error) {
      translationErrorBySession[statusKey] = payload.error;
    } else {
      delete translationErrorBySession[statusKey];
    }
  })
    .then((unlisten) => {
      unlistenTranslationStatus = unlisten;
    })
    .catch(() => {});
});
onUnmounted(() => {
  window.removeEventListener("keydown", handleWindowKeydown);
  if (searchDebounceTimer) clearTimeout(searchDebounceTimer);
  if (expandedFoldersPersistTimer) clearTimeout(expandedFoldersPersistTimer);
  unlistenNotesStatus?.();
  unlistenTranslationStatus?.();
  recordingGeneration += 1;
  if (recording.value || recordingStarting.value)
    void invoke("cancel_recording", { sessionId: recordingSessionId.value }).catch(
      () => undefined,
    );
  revokeImportedAudio();
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
        <ActivityBar :active-view="activeView" @select="selectActivityView" />
        <ExplorerView
          v-if="sidebarOpen && activeView === 'explorer'"
          ref="explorerViewRef"
          :nodes="data.nodes"
          :open-node-id="openNodeId"
          :expanded-folders="expandedFolders"
          :operation-busy="operationBusy"
          :language="data.settings.language"
          :focused-node-id="focusedNodeId"
          :context-menu="contextMenu"
          :inline-create="inlineCreate"
          :create-draft="createDraft"
          :renaming-id="renamingId"
          :rename-draft="renameDraft"
          :delete-target="deleteTarget"
          :drag-node-id="dragNodeId"
          :drop-parent="dropParent"
          @toggle-folder="toggleFolder"
          @open-node="openNode($event)"
          @focus-node="focusNode"
          @context-menu="openContextMenu"
          @close-context-menu="closeContextMenu"
          @start-create="startCreate"
          @cancel-create="cancelCreate"
          @commit-create="commitCreate"
          @update:create-draft="createDraft = $event"
          @start-import="startImportMaterial"
          @start-rename="startRename"
          @cancel-rename="cancelRename"
          @commit-rename="commitRename"
          @update:rename-draft="renameDraft = $event"
          @open-delete-dialog="openDeleteDialog"
          @cancel-delete="cancelDelete"
          @confirm-delete="confirmDelete"
          @drag-start="handleDragStart"
          @drag-over="handleDragOver"
          @drag-leave="handleDragLeave"
          @drag-end="handleDragEnd"
          @drop="handleNodeDrop"
          @auto-expand-folder="autoExpandFolder"
        />
        <SearchView
          v-if="sidebarOpen && activeView === 'search'"
          ref="searchViewRef"
          :nodes="data.nodes"
          :query="searchQuery"
          :results="searchResults"
          :operation-busy="operationBusy"
          @update:query="searchQuery = $event"
          @open-node="openFromSearch"
        />
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
            <h1>{{ (session && nodeName(openNodeId)) || t("appTitle") }}</h1>
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
            <WorkbenchChat
              ref="workbenchChatRef"
              v-model:active-tab="activeSessionTab"
              v-model:draft="draft"
              v-model:edit-draft="editDraft"
              :rendered-messages="renderedMessages"
              :editing-message-id="editingMessageId"
              :operation-busy="operationBusy"
              :streaming="streaming"
              :has-provider-key="Boolean(currentProvider?.hasKey)"
              :summary-loading="summaryLoading"
              :summary-stream="summaryStream"
              :summary-updated-label="summaryUpdatedLabel"
              :session-summary="session.summary"
              :summary-html="summaryHtml"
              :notes-enabled="notesEnabled"
              :notes-status="notesStatus"
              @content-click="handleContentClick"
              @update:notes-enabled="toggleNotesEnabled"
              @copy-message="copyMessage"
              @start-edit-message="startEditMessage"
              @cancel-edit-message="cancelEditMessage"
              @commit-edit-message="commitEditMessage"
              @regenerate-message="regenerateMessage"
              @delete-message="deleteMessage"
              @submit-message="send"
              @cancel="cancel"
              @composer-enter="handleComposerEnter"
              @composer-input="handleComposerInput"
              @ask-uncertain="askUncertain"
              @summarize="summarize"
            />
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
            <TranscriptPanel
              ref="transcriptPanelRef"
              :stt-ready="stt.ready"
              :transcription-loading="transcriptionLoading"
              :operation-busy="operationBusy"
              :has-session="Boolean(session)"
              :transcription="session?.transcription"
              :recording-starting="recordingStarting"
              :recording="recording"
              :recording-session-id="recordingSessionId"
              :streaming="streaming"
              :summary-loading="summaryLoading"
              :capture-mode="captureMode"
              :system-audio-capability="systemAudioCapability"
              :language="transcriptionLanguage"
              :recording-level="recordingLevel"
              :audio-url="importedAudioUrl"
              :translation-enabled="translationEnabled"
              :translation-target-language="translationTargetLanguage"
              :translation-mode="translationMode"
              :translator-ready="providerConnected"
              :translation-error="translationError"
              :sentence-translations="currentSentenceTranslations"
              :translating-sentences="currentTranslatingSentences"
              @upload-audio="uploadAudio"
              @cancel="cancel"
              @toggle-recording="toggleRecording"
              @update:capture-mode="captureMode = $event"
              @update:language="transcriptionLanguage = $event"
              @update:translation-enabled="(value) => setTranslationSettings({ enabled: value })"
              @update:translation-target-language="
                (value) => setTranslationSettings({ targetLanguage: value })
              "
              @update:translation-mode="(value) => setTranslationSettings({ mode: value })"
            />
          </template>
          <section v-else class="main-workspace">
            <div v-if="material" class="material-editor-panel">
              <h2>{{ nodeName(openNodeId) }}</h2>
              <FluentTextArea
                v-model="materialDraft"
                :label="t('material.contentLabel')"
                class="material-editor"
              />
              <div class="material-actions">
                <FluentButton tone="secondary" @click="saveMaterial"
                  >{{ t("common.save") }}</FluentButton
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
    </main>
  </FluentTheme>
</template>
