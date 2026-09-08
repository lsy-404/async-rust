<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import MarkdownIt from "markdown-it";
import { invoke, Channel } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { ModelAuthDialog, type ModelAuthProvider } from "@model-auth/vue";
import {
  FluentButton,
  FluentDialog,
  FluentField,
  FluentNotice,
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
  Workspace,
} from "./types";
import {
  cancelStream,
  removeProviderKey,
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
const authBusy = ref(false);
const authProgress = ref("");
const stt = ref<SttStatus>({
  ready: false,
  modelName: "本地语音模型",
  modelPath: "",
  sizeBytes: 0,
});
const sttDownloading = ref(false);
const sttProgress = ref({ downloaded: 0, total: 0 });
const settingsOpen = ref(false);
const authOpen = ref(false);
const workspaceName = ref("");
const sessionTitle = ref("");
const materialDraft = ref("");
const recording = ref(false);
const providerName = ref("");
const providerEndpoint = ref("");
const deleteTarget = ref<{
  kind: "workspace" | "session" | "material" | "provider";
  id: string;
  label: string;
}>();
let recorder: MediaRecorder | undefined;
let chunks: Blob[] = [];

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
    recording.value,
);
const modelOptions = computed(() =>
  (currentProvider.value?.models ?? []).map((model) => ({
    value: model,
    label: model,
  })),
);
const authProviders = computed<ModelAuthProvider[]>(() =>
  data.value.providers.map((provider) => ({
    id: provider.id,
    name: provider.name,
    description: provider.baseUrl,
    authMethods: [provider.authMethod === "oauth" ? "oauth" : "api-key"],
    available: true,
    models: provider.models,
    apiKeyModels: provider.models,
    apiKeyCredentials:
      provider.authMethod !== "oauth" && provider.hasKey
        ? [
            {
              id: "primary",
              label: "API key",
              healthy: true,
              enabled: true,
              weight: 1,
              models: provider.models,
            },
          ]
        : [],
    oauthEnabled: provider.authMethod === "oauth",
    oauthCredentials:
      provider.authMethod === "oauth" && provider.hasKey
        ? [
            {
              id: "primary",
              label: "已连接",
              healthy: true,
              enabled: true,
              weight: 1,
              models: provider.models,
            },
          ]
        : [],
  })),
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
async function refresh(preferred?: { providerId?: string; model?: string }) {
  loading.value = true;
  error.value = "";
  try {
    data.value = await invoke<AppData>("load_state");
    stt.value = await invoke<SttStatus>("stt_status");
    if (preferred?.providerId)
      data.value.settings.providerId = preferred.providerId;
    if (preferred?.model !== undefined)
      data.value.settings.model = preferred.model;
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
async function createWorkspace() {
  if (!workspaceName.value.trim()) return;
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
  kind: "workspace" | "session" | "material" | "provider",
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
    if (target.kind === "provider")
      await invoke("delete_provider", { id: target.id });
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
async function removeKey(providerId: string) {
  const provider = data.value.providers.find((item) => item.id === providerId);
  if (!provider) return;
  try {
    await removeProviderKey(invoke, provider);
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
          extensions: ["mp3", "m4a", "mp4", "wav", "webm", "ogg"],
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
    transcriptionLoading.value
  )
    return;
  if (recorder && recording.value) {
    recorder.stop();
    return;
  }
  const sessionId = session.value.id;
  let stream: MediaStream | undefined;
  try {
    stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    chunks = [];
    recorder = new MediaRecorder(stream);
    recorder.ondataavailable = (event) => chunks.push(event.data);
    recorder.onstop = async () => {
      recording.value = false;
      transcriptionLoading.value = true;
      stream?.getTracks().forEach((track) => track.stop());
      const mime = recorder?.mimeType || "audio/webm";
      const extension = mime.includes("mp4")
        ? "m4a"
        : mime.includes("ogg")
          ? "ogg"
          : "webm";
      try {
        const bytes = Array.from(
          new Uint8Array(await new Blob(chunks, { type: mime }).arrayBuffer()),
        );
        await invoke("transcribe_bytes", {
          sessionId,
          name: `recording.${extension}`,
          bytes,
        });
        await refresh();
      } catch (cause) {
        report(cause);
      } finally {
        transcriptionLoading.value = false;
      }
    };
    recorder.start();
    recording.value = true;
  } catch (cause) {
    stream?.getTracks().forEach((track) => track.stop());
    report(`Microphone permission is required: ${String(cause)}`);
  }
}
async function saveSettings() {
  try {
    if (currentProvider.value)
      await invoke("save_provider", {
        provider: currentProvider.value,
        apiKey: null,
      });
    await invoke("save_settings", { settings: data.value.settings });
    settingsOpen.value = false;
    notice.value = "Settings saved locally.";
    await refresh();
  } catch (cause) {
    report(cause);
  }
}
async function discover() {
  if (!currentProvider.value) return;
  try {
    const models = await invoke<string[]>("discover_models", {
      providerId: currentProvider.value.id,
    });
    currentProvider.value.models = models;
    if (!data.value.settings.model) data.value.settings.model = models[0] ?? "";
  } catch (cause) {
    report(cause);
  }
}
async function saveKey(payload: { providerId: string; apiKey: string }) {
  const provider = data.value.providers.find(
    (item) => item.id === payload.providerId,
  );
  if (!provider) return;
  authBusy.value = true;
  try {
    await invoke("save_provider", { provider, apiKey: payload.apiKey });
    await invoke("save_settings", { settings: data.value.settings });
    await discover();
    await refresh({
      providerId: data.value.settings.providerId,
      model: data.value.settings.model,
    });
  } catch (cause) {
    report(cause);
  } finally {
    authBusy.value = false;
  }
}
async function authorizeOAuth(providerId: string) {
  authBusy.value = true;
  authProgress.value = "正在打开供应商授权…";
  const channel = new Channel<{ type: "status" | "url"; text: string }>();
  channel.onmessage = (event) => {
    authProgress.value = event.text;
  };
  try {
    await invoke("authorize_oauth", { providerId, onEvent: channel });
    await refresh();
  } catch (cause) {
    report(cause);
  } finally {
    authBusy.value = false;
  }
}
async function removeOAuth(providerId: string) {
  authBusy.value = true;
  try {
    await invoke("remove_oauth", { providerId });
    await refresh();
  } catch (cause) {
    report(cause);
  } finally {
    authBusy.value = false;
  }
}
async function cancelOAuth(providerId: string) {
  try {
    await invoke("cancel_oauth", { providerId });
  } catch (cause) {
    report(cause);
  } finally {
    authBusy.value = false;
  }
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
    data.value.settings.providerId = provider.id;
    await refresh({ providerId: provider.id });
  } catch (cause) {
    report(cause);
  }
}
async function selectModel(selection: { providerId: string; model: string }) {
  data.value.settings.providerId = selection.providerId;
  data.value.settings.model = selection.model;
  try {
    await invoke("save_settings", { settings: data.value.settings });
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
onUnmounted(() => recorder?.state === "recording" && recorder.stop());
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
                  tone="subtle"
                  :busy="transcriptionLoading"
                  :disabled="operationBusy"
                  @click="uploadAudio"
                  >导入音频</FluentButton
                ><FluentButton
                  :tone="recording ? 'danger' : 'subtle'"
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
                @keydown.enter.exact.prevent="send"
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
            <FluentSelect
              v-model="data.settings.providerId"
              label="模型供应商"
              :options="
                data.providers.map((p) => ({ value: p.id, label: p.name }))
              "
            /><FluentField
              v-if="currentProvider"
              v-model="currentProvider.baseUrl"
              label="兼容 API 地址"
              placeholder="https://…/v1"
            /><FluentSelect
              v-model="data.settings.model"
              label="对话模型"
              :options="modelOptions"
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
              <progress
                v-if="sttDownloading"
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
            <div class="settings-actions">
              <FluentButton tone="secondary" @click="authOpen = true"
                >连接 API Key</FluentButton
              ><FluentButton tone="subtle" @click="discover"
                >获取模型</FluentButton
              ><FluentButton
                v-if="currentProvider && currentProvider.id !== 'openai'"
                tone="danger"
                @click="
                  askDelete(
                    'provider',
                    currentProvider.id,
                    currentProvider.name,
                  )
                "
                >删除供应商</FluentButton
              >
            </div>
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
            ><FluentSelect
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
      <ModelAuthDialog
        :open="authOpen"
        :providers="authProviders"
        :model="{
          providerId: data.settings.providerId,
          model: data.settings.model,
        }"
        :theme="data.settings.theme"
        initial-method="api-key"
        :busy="authBusy"
        :error="error || null"
        :catalog-status="{ state: 'ready', source: 'cached' }"
        @close="authOpen = false"
        @add-api-key="saveKey"
        @remove-api-key="removeKey"
        @authorize-oauth="authorizeOAuth"
        @reconnect-oauth="authorizeOAuth"
        @remove-oauth="removeOAuth"
        @refresh-catalog="discover"
        @select-model="selectModel"
      />
    </main>
  </FluentTheme>
</template>
