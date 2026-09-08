<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref, watch } from "vue";
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
  Provider,
  Session,
  Settings,
  StreamEvent,
  SttStatus,
  RecordingEvent,
  Workspace,
} from "./types";
import {
  cancelStream,
  saveNewProvider,
  streamCommand,
} from "./workbench-commands";

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
const modelConnections = ref<InstanceType<typeof ModelConnections>>();
const workspaceName = ref("");
const sessionTitle = ref("");
const materialDraft = ref("");
const recording = ref(false);
const providerName = ref("");
const providerEndpoint = ref("");
const deleteTarget = ref<{
  kind: "workspace" | "session" | "material";
  id: string;
  label: string;
}>();
const recordingStarting = ref(false);
const recordingSessionId = ref("");
let recordingGeneration = 0;

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
async function createSession() {
  if (!selectedWorkspaceId.value || !sessionTitle.value.trim()) return;
  try {
    const created = await invoke<Session>("create_session", {
      workspaceId: selectedWorkspaceId.value,
      title: sessionTitle.value.trim(),
    });
    sessionTitle.value = "";
    selectedSessionId.value = created.id;
    await refresh();
  } catch (cause) {
    report(cause);
  }
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
    if (generation !== recordingGeneration || event.sessionId !== sessionId) return;
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
async function createProvider() {
  if (!providerName.value.trim() || !providerEndpoint.value.trim()) return;
  const provider: Provider = {
    id: crypto.randomUUID(),
    name: providerName.value.trim(),
    baseUrl: providerEndpoint.value.trim(),
    models: [],
    hasKey: false,
  };
  try {
    await saveNewProvider(invoke, provider, data.value.settings);
    providerName.value = "";
    providerEndpoint.value = "";
    await refresh();
    await modelConnections.value?.refresh();
  } catch (cause) {
    report(cause);
  }
}
watch(selectedWorkspaceId, () => {
  if (!operationBusy.value) {
    selectedSessionId.value = sessions.value[0]?.id ?? "";
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
onMounted(refresh);
onUnmounted(() => {
  recordingGeneration += 1;
  if (recording.value || recordingStarting.value)
    void invoke("cancel_recording").catch(() => undefined);
});
</script>

<template>
  <FluentTheme :mode="data.settings.theme">
    <main class="app-shell">
      <header class="titlebar">
        <div>
          <strong>课堂工作台</strong><span>本地保存 · 你的模型密钥</span>
        </div>
        <div class="toolbar">
          <span v-if="currentProvider" class="status">{{
            currentProvider.hasKey ? currentProvider.name : "需要连接模型"
          }}</span
          ><FluentButton tone="subtle" @click="settingsOpen = true"
            >设置</FluentButton
          >
        </div>
      </header>
      <FluentNotice v-if="error" tone="danger" class="notice"
        >{{ error }} <button @click="error = ''">关闭</button></FluentNotice
      >
      <FluentNotice v-if="notice" tone="success" class="notice"
        >{{ notice }} <button @click="notice = ''">关闭</button></FluentNotice
      >
      <div v-if="loading" class="loading">正在加载本地课堂数据…</div>
      <div v-else class="workbench">
        <aside class="sidebar">
          <section>
            <h2>工作区</h2>
            <form class="inline-form" @submit.prevent="createWorkspace">
              <input v-model="workspaceName" placeholder="新工作区" /><button
                aria-label="Create workspace"
              >
                +
              </button>
            </form>
            <div v-if="!workspaces.length" class="empty">
              创建一个工作区开始整理课堂。
            </div>
            <button
              v-for="item in workspaces"
              :key="item.id"
              class="nav-row"
              :class="{ active: item.id === selectedWorkspaceId }"
              :disabled="operationBusy"
              @click="selectedWorkspaceId = item.id"
            >
              <span>{{ item.name }}</span
              ><i @click.stop="askDelete('workspace', item.id, item.name)">×</i>
            </button>
          </section>
          <section>
            <h2>会话</h2>
            <form class="inline-form" @submit.prevent="createSession">
              <input
                v-model="sessionTitle"
                :disabled="!selectedWorkspaceId || operationBusy"
                placeholder="新会话标题"
              /><button
                :disabled="!selectedWorkspaceId || operationBusy"
                aria-label="Create session"
              >
                +
              </button>
            </form>
            <div v-if="selectedWorkspaceId && !sessions.length" class="empty">
              此工作区还没有会话。
            </div>
            <button
              v-for="item in sessions"
              :key="item.id"
              class="nav-row"
              :class="{ active: item.id === selectedSessionId }"
              :disabled="operationBusy"
              @click="selectedSessionId = item.id"
            >
              <span>{{ item.title }}</span
              ><i @click.stop="askDelete('session', item.id, item.title)">×</i>
            </button>
          </section>
        </aside>
        <section class="chat-panel">
          <template v-if="session"
            ><header class="panel-header">
              <div>
                <h1>{{ session.title }}</h1>
                <p>
                  {{
                    currentProvider?.hasKey
                      ? data.settings.model || "选择模型"
                      : "先在设置中连接模型"
                  }}
                </p>
              </div>
              <div>
                <FluentButton
                  v-if="summaryLoading"
                  tone="danger"
                  @click="cancel"
                  >停止摘要</FluentButton
                ><FluentButton
                  v-else
                  tone="secondary"
                  :disabled="operationBusy"
                  @click="summarize"
                  >生成摘要</FluentButton
                ><FluentButton
                  v-if="transcriptionLoading"
                  tone="danger"
                  @click="cancel"
                  >取消本地转写</FluentButton
                ><FluentButton
                  tone="subtle"
                  :busy="transcriptionLoading"
                  :disabled="operationBusy"
                  @click="uploadAudio"
                  >{{
                    transcriptionLoading ? "本地转写中…" : "导入音频"
                  }}</FluentButton
                ><FluentButton
                  :tone="recording ? 'danger' : 'subtle'"
                  :busy="recordingStarting"
                  :disabled="
                    streaming || summaryLoading || transcriptionLoading
                  "
                  @click="toggleRecording"
                  >{{ recording ? "停止录音" : "录音转写" }}</FluentButton
                >
              </div>
            </header>
            <div class="transcript" v-if="session.transcription">
              <strong>课堂转写</strong>
              <p>{{ session.transcription }}</p>
            </div>
            <div class="messages">
              <article
                v-for="message in renderedMessages"
                :key="message.id"
                :class="['message', message.role]"
              >
                <label>{{ message.role === "user" ? "你" : "课堂助手" }}</label>
                <div v-html="message.html"></div>
              </article>
              <div v-if="!renderedMessages.length" class="empty center">
                向课堂助手提问，它会结合当前工作区的材料和转写内容。
              </div>
            </div>
            <div v-if="session.summary || summaryStream" class="summary">
              <strong>课堂摘要</strong>
              <div
                v-html="md.render(summaryStream || session.summary || '')"
              ></div>
            </div>
            <form class="composer" @submit.prevent="send">
              <textarea
                v-model="draft"
                :disabled="operationBusy"
                placeholder="输入问题，Enter 发送，Shift+Enter 换行"
                @keydown.enter.exact.prevent="handleComposerEnter"
              ></textarea
              ><FluentButton v-if="streaming" tone="danger" @click="cancel"
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
          <div v-else class="empty center">
            选择或创建一个会话以开始课堂对话。
          </div>
        </section>
        <aside class="materials">
          <header>
            <h2>材料</h2>
            <FluentButton
              tone="subtle"
              :disabled="!selectedWorkspaceId"
              @click="importMaterial"
              >导入</FluentButton
            >
          </header>
          <div v-if="selectedWorkspaceId && !materials.length" class="empty">
            导入 TXT、Markdown 或 DOCX 材料，供会话参考。
          </div>
          <button
            v-for="item in materials"
            :key="item.id"
            class="material-row"
            :class="{ active: item.id === selectedMaterialId }"
            @click="selectedMaterialId = item.id"
          >
            {{ item.name }}</button
          ><template v-if="material"
            ><FluentTextArea
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
            </div></template
          >
        </aside>
      </div>
      <FluentDialog v-model:open="settingsOpen" label="Settings"
        ><template #default
          ><div class="settings">
            <h2>模型与外观</h2>
            <ModelConnections
              ref="modelConnections"
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
            <details class="custom-provider-settings">
              <summary>自定义供应商</summary>
              <FluentField
                v-model="providerName"
                label="新增供应商名称"
                placeholder="例如本地模型服务"
              /><FluentField
                v-model="providerEndpoint"
                label="新增供应商 API 地址"
                placeholder="https://…/v1"
              /><FluentButton
                tone="secondary"
                :disabled="!providerName.trim() || !providerEndpoint.trim()"
                @click="createProvider"
                >新增供应商</FluentButton
              >
            </details>
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
          ><FluentButton tone="primary" @click="saveSettings"
            >保存设置</FluentButton
          ></template
        ></FluentDialog
      >
      <FluentDialog
        :open="Boolean(deleteTarget)"
        label="Confirm deletion"
        @update:open="
          (open) => {
            if (!open) deleteTarget = undefined;
          }
        "
        ><template #default
          ><p>删除“{{ deleteTarget?.label }}”后无法恢复。继续吗？</p></template
        ><template #footer
          ><FluentButton tone="subtle" @click="deleteTarget = undefined"
            >取消</FluentButton
          ><FluentButton tone="danger" @click="confirmDelete"
            >删除</FluentButton
          ></template
        ></FluentDialog
      >
    </main>
  </FluentTheme>
</template>
