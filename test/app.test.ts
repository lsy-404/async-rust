import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";
import App from "../src/App.vue";
import ModelConnections from "../src/components/ModelConnections.vue";
import type { AppData } from "../src/types";

const { invoke, listen } = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke,
  Channel: class {
    onmessage?: (event: unknown) => void;
  },
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));
const state = (): AppData => ({
  nodes: [{ id: "s1", parentId: null, kind: "session", name: "Lesson" }],
  sessions: [
    {
      id: "s1",
      messages: [],
      transcription: "",
      summary: "",
    },
  ],
  materials: [],
  settings: {
    providerId: "openai",
    model: "classroom-test",
    theme: "system",
    language: "zh",
  },
  providers: [
    {
      id: "openai",
      name: "OpenAI",
      baseUrl: "http://fixture/v1",
      models: ["classroom-test"],
      hasKey: true,
    },
  ],
});

const stubs = {
  FluentTheme: { template: "<div><slot /></div>" },
  FluentButton: { template: "<button v-bind='$attrs'><slot /></button>" },
  FluentNotice: { template: "<div><slot /></div>" },
  FluentDialog: { template: "<div><slot /><slot name='footer' /></div>" },
  FluentTextArea: {
    props: ["modelValue", "label"],
    emits: ["update:modelValue"],
    template:
      "<label>{{ label }}<textarea :value='modelValue' @input='$emit(\"update:modelValue\", $event.target.value)' /></label>",
  },
  FluentSelect: {
    props: ["modelValue", "label", "options"],
    emits: ["update:modelValue"],
    template:
      "<label>{{ label }}<select :aria-label='label' :value='modelValue' @change='$emit(\"update:modelValue\", $event.target.value)'><option v-for='option in options' :value='option.value'>{{ option.label }}</option></select></label>",
  },
  ModelConnections: {
    props: ["theme", "refreshWorkbench"],
    methods: { refresh: () => Promise.resolve(true) },
    template: "<div data-test='model-connections' />",
  },
};

function mountApp() {
  return mount(App, { global: { stubs } });
}

function buttonWithText(wrapper: ReturnType<typeof mountApp>, text: string) {
  const button = wrapper.findAll("button").find((item) => item.text() === text);
  if (!button) throw new Error(`Missing button: ${text}`);
  return button;
}
async function openSessionNode(
  wrapper: ReturnType<typeof mountApp>,
  name: string,
) {
  const button = wrapper
    .findAll(".tree-session")
    .find((item) => item.text() === name);
  if (!button) throw new Error(`Missing session node: ${name}`);
  await button.trigger("click");
  await flushPromises();
}

describe("desktop workbench interactions", () => {
  let data: AppData;
  beforeEach(() => {
    data = state();
    invoke.mockReset();
    listen.mockReset();
    listen.mockResolvedValue(() => {});
    invoke.mockImplementation(
      async (command: string, args?: Record<string, unknown>) => {
        if (command === "load_state") return structuredClone(data);
        if (command === "stt_status")
          return {
            ready: true,
            modelName: "fixture",
            modelPath: "/tmp/model",
            sizeBytes: 1,
          };
        if (command === "save_settings")
          data.settings = { ...(args?.settings as AppData["settings"]) };
        return undefined;
      },
    );
  });

  it("uses the Kit connection panel as the only model-provider entry point", async () => {
    const wrapper = mountApp();
    await flushPromises();
    expect(wrapper.find('[data-test="model-connections"]').exists()).toBe(true);
    expect(wrapper.find('[aria-label="新增供应商名称"]').exists()).toBe(false);
    expect(wrapper.find('[aria-label="新增供应商 API 地址"]').exists()).toBe(false);
    expect(wrapper.text()).not.toContain("新增供应商");
    expect(wrapper.find('select[aria-label="模型供应商"]').exists()).toBe(
      false,
    );
  });

  it("saves appearance settings without rewriting the OAuth provider", async () => {
    data.providers[0] = {
      ...data.providers[0]!,
      id: "workbuddy",
      authMethod: "oauth",
    };
    data.settings.providerId = "workbuddy";
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Lesson");
    expect(wrapper.find('input[aria-label="兼容 API 地址"]').exists()).toBe(
      false,
    );
    await buttonWithText(wrapper, "保存设置").trigger("click");
    await flushPromises();
    expect(
      invoke.mock.calls.some(([command]) => command === "save_provider"),
    ).toBe(false);
    expect(data.settings.providerId).toBe("workbuddy");
  });

  it("refreshes the active model after native auth without dropping the header theme choice", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Lesson");
    await buttonWithText(wrapper, "深色").trigger("click");
    await flushPromises();
    data.settings.model = "native-model";
    data.providers[0]!.name = "Native connection";
    const connections = wrapper.getComponent(ModelConnections);
    await connections.props("refreshWorkbench")();
    await flushPromises();
    expect(wrapper.get(".status").text()).toBe("Native connection");
    expect(connections.props("theme")).toBe("dark");
    await buttonWithText(wrapper, "保存设置").trigger("click");
    await flushPromises();
    expect(data.settings.model).toBe("native-model");
    expect(data.settings.theme).toBe("dark");
  });

  it("renders chat deltas before completion and keeps the user message on stream error", async () => {
    let rejectChat: (cause: Error) => void = () => undefined;
    invoke.mockImplementation((command: string, args?: Record<string, any>) => {
      if (command === "stt_status")
        return Promise.resolve({
          ready: true,
          modelName: "fixture",
          modelPath: "/tmp/model",
          sizeBytes: 1,
        });
      if (command === "load_state")
        return Promise.resolve(structuredClone(data));
      if (command === "chat") {
        args?.onEvent.onmessage({ type: "delta", text: "partial response" });
        return new Promise((_, reject) => {
          rejectChat = reject;
        });
      }
      return Promise.resolve(undefined);
    });
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Lesson");
    await wrapper.get("textarea").setValue("Explain this");
    await wrapper.get("form.composer").trigger("submit");
    await nextTick();
    expect(wrapper.text()).toContain("partial response");
    rejectChat(new Error("fixture unavailable"));
    await flushPromises();
    expect(wrapper.text()).toContain("Explain this");
    expect(wrapper.text()).toContain("fixture unavailable");
  });

  it("shows native download progress through Fluent and forwards cancellation", async () => {
    const implementation = invoke.getMockImplementation()!;
    let rejectDownload: (error: Error) => void = () => undefined;
    invoke.mockImplementation((command, args) => {
      if (command === "stt_status")
        return Promise.resolve({
          ready: false,
          modelName: "fixture",
          modelPath: "",
          sizeBytes: 0,
        });
      if (command === "download_stt_model") {
        args.onEvent.onmessage({ downloaded: 25, total: 100 });
        return new Promise((_, reject) => {
          rejectDownload = reject;
        });
      }
      if (command === "cancel_stt_download")
        rejectDownload(new Error("download cancelled"));
      return implementation(command, args);
    });
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Lesson");
    await buttonWithText(wrapper, "下载本地转写模型").trigger("click");
    await flushPromises();
    const progress = wrapper.get(".fluent-progress-bar");
    expect(progress.attributes("aria-valuenow")).toBe("25");
    expect(progress.attributes("aria-label")).toBe("本地语音模型下载进度");
    await buttonWithText(wrapper, "取消下载").trigger("click");
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("cancel_stt_download");
    expect(wrapper.find(".fluent-progress-bar").exists()).toBe(false);
    wrapper.unmount();
  });

  it("pins native microphone capture to the original session and unlocks after a stop error", async () => {
    data.nodes.push({ id: "s2", parentId: null, kind: "session", name: "Lesson 2" });
    data.sessions.push({ id: "s2", messages: [], transcription: "", summary: "" });
    const implementation = invoke.getMockImplementation()!;
    invoke.mockImplementation((command, args) =>
      command === "stop_recording"
        ? Promise.reject(new Error("native microphone failed"))
        : implementation(command, args),
    );
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Lesson");
    await buttonWithText(wrapper, "开始录音").trigger("click");
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("start_recording", {
      sessionId: "s1",
      onEvent: expect.objectContaining({ onmessage: expect.any(Function) }),
      source: "microphone",
      language: null,
    });
    expect(
      wrapper
        .findAll(".tree-session")
        .find((item) => item.text() === "Lesson 2")
        ?.attributes("disabled"),
    ).toBeDefined();
    await buttonWithText(wrapper, "停止录音").trigger("click");
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("stop_recording", { sessionId: "s1" });
    expect(wrapper.text()).toContain("native microphone failed");
    expect(
      wrapper
        .findAll(".tree-session")
        .find((item) => item.text() === "Lesson 2")
        ?.attributes("disabled"),
    ).toBeUndefined();
  });

  it("renders native transcript updates before stop and ignores foreign or stale events", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Lesson");
    await buttonWithText(wrapper, "开始录音").trigger("click");
    await flushPromises();
    const start = invoke.mock.calls.find(
      ([command]) => command === "start_recording",
    )!;
    const channel = start[1].onEvent;
    channel.onmessage({
      type: "transcript",
      sessionId: "s1",
      text: "录音中的第一句话。",
    });
    await nextTick();
    expect(wrapper.text()).toContain("录音中的第一句话。");
    expect(
      invoke.mock.calls.some(([command]) => command === "stop_recording"),
    ).toBe(false);
    channel.onmessage({
      type: "transcript",
      sessionId: "foreign",
      text: "其他会话的内容",
    });
    await nextTick();
    expect(wrapper.text()).not.toContain("其他会话的内容");
    data.sessions[0].transcription = "录音中的第一句话。第二句话。";
    channel.onmessage({
      type: "transcript",
      sessionId: "s1",
      text: data.sessions[0].transcription,
    });
    await nextTick();
    expect(wrapper.text().match(/录音中的第一句话/g)).toHaveLength(1);
    await buttonWithText(wrapper, "停止录音").trigger("click");
    await flushPromises();
    channel.onmessage({
      type: "transcript",
      sessionId: "s1",
      text: "过期事件",
    });
    await nextTick();
    expect(wrapper.text()).toContain("第二句话。");
    expect(wrapper.text()).not.toContain("过期事件");
    wrapper.unmount();
  });

  it("surfaces an asynchronous recording error and unlocks recording", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Lesson");
    await buttonWithText(wrapper, "开始录音").trigger("click");
    await flushPromises();
    const start = invoke.mock.calls.find(
      ([command]) => command === "start_recording",
    )!;
    start[1].onEvent.onmessage({
      type: "error",
      sessionId: "s1",
      text: "音频处理队列已满",
    });
    await flushPromises();
    expect(wrapper.text()).toContain("音频处理队列已满");
    expect(invoke).toHaveBeenCalledWith("cancel_recording", { sessionId: "s1" });
    expect(
      buttonWithText(wrapper, "开始录音").attributes("disabled"),
    ).toBeUndefined();
    wrapper.unmount();
  });

  it("shows the auto-notes toggle on by default and can turn it off", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Lesson");
    await buttonWithText(wrapper, "摘要").trigger("click");
    await nextTick();
    const toggle = wrapper.find('button[role="switch"]');
    expect(toggle.exists()).toBe(true);
    expect(toggle.attributes("aria-checked")).toBe("true");

    await toggle.trigger("click");
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("set_notes_enabled", {
      id: "s1",
      enabled: false,
    });
    expect(toggle.attributes("aria-checked")).toBe("false");
    wrapper.unmount();
  });

  it("reflects background notes status pushed through the notes-status event", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Lesson");
    await buttonWithText(wrapper, "摘要").trigger("click");
    await nextTick();
    const handler = listen.mock.calls.find(
      ([name]) => name === "notes-status",
    )![1];

    handler({ payload: { sessionId: "s1", status: "generating" } });
    await nextTick();
    expect(wrapper.text()).toContain("正在自动生成笔记");

    handler({
      payload: {
        sessionId: "s1",
        status: "idle",
        summary: "自动生成的课堂笔记。",
        summaryUpdatedAt: "2024-03-05T10:00:00.000Z",
      },
    });
    await nextTick();
    expect(wrapper.text()).toContain("自动生成的课堂笔记。");
    wrapper.unmount();
  });

  it("keeps live translation off by default and persists the toggle, target language and view mode", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Lesson");
    // Only the transcript panel's switch exists until the summary tab (which
    // hosts the notes switch) is opened, so this is unambiguously it.
    const toggle = wrapper.get('button[role="switch"]');
    expect(toggle.attributes("aria-checked")).toBe("false");
    expect(wrapper.find('select[aria-label="翻译目标语言"]').exists()).toBe(false);

    await toggle.trigger("click");
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("set_translation_settings", {
      id: "s1",
      enabled: true,
      targetLanguage: null,
      mode: "side-by-side",
    });
    expect(toggle.attributes("aria-checked")).toBe("true");
    // Defaults to the app's own UI language (zh in the fixture) until a
    // target language is explicitly chosen.
    expect(
      (wrapper.get('select[aria-label="翻译目标语言"]').element as HTMLSelectElement).value,
    ).toBe("zh");

    await wrapper
      .get('select[aria-label="翻译目标语言"]')
      .setValue("en");
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("set_translation_settings", {
      id: "s1",
      enabled: true,
      targetLanguage: "en",
      mode: "side-by-side",
    });

    await wrapper.get('[aria-label="翻译显示方式"] button:last-child').trigger("click");
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("set_translation_settings", {
      id: "s1",
      enabled: true,
      targetLanguage: "en",
      mode: "separate",
    });
    wrapper.unmount();
  });

  it("batches finalized live sentences into one queue call, holding back the still-growing last one", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Lesson");
    await wrapper.get('button[role="switch"]').trigger("click");
    await flushPromises();
    invoke.mockClear();

    await buttonWithText(wrapper, "开始录音").trigger("click");
    await flushPromises();
    const start = invoke.mock.calls.find(
      ([command]) => command === "start_recording",
    )!;
    invoke.mockClear();
    start[1].onEvent.onmessage({
      type: "transcript",
      sessionId: "s1",
      text: "First sentence. Second sentence. Still growing",
    });
    await flushPromises();

    const queued = invoke.mock.calls.filter(
      ([command]) => command === "queue_sentence_translations",
    );
    expect(queued).toHaveLength(1);
    expect(queued[0]![1]).toEqual({
      sessionId: "s1",
      targetLanguage: "zh",
      sentences: ["First sentence.", "Second sentence."],
    });
    wrapper.unmount();
  });

  it("shows a cache hit immediately and clears the translating state from the translation-status event", async () => {
    invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "load_state") return structuredClone(data);
      if (command === "stt_status")
        return { ready: true, modelName: "fixture", modelPath: "/tmp/model", sizeBytes: 1 };
      if (command === "queue_sentence_translations") {
        const sentences = args?.sentences as string[];
        // "Cached." is already cached server-side and comes back as an
        // immediate hit; "Fresh." is not, and stays pending for the event.
        const hits: Record<string, string> = {};
        if (sentences.includes("Cached.")) hits["Cached."] = "缓存翻译。";
        return hits;
      }
      return undefined;
    });
    data.sessions[0]!.transcription = "Cached. Fresh.";
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Lesson");
    await wrapper.get('button[role="switch"]').trigger("click");
    await flushPromises();

    // The cache hit for "Cached." renders immediately, from the queue call's
    // own return value - no need to wait for any event.
    expect(wrapper.text()).toContain("缓存翻译。");
    // "Fresh." has no cache hit, so it must show the translating state until
    // the translation-status event resolves it.
    expect(wrapper.text()).toContain("翻译中");

    const handler = listen.mock.calls.find(
      ([name]) => name === "translation-status",
    )![1];
    handler({
      payload: {
        sessionId: "s1",
        targetLanguage: "zh",
        sentences: ["Fresh."],
        translations: { "Fresh.": "全新翻译。" },
        error: null,
      },
    });
    await nextTick();
    expect(wrapper.text()).toContain("全新翻译。");
    expect(wrapper.text()).not.toContain("翻译中");
    wrapper.unmount();
  });

  it("surfaces a translation-status event's error instead of silently clearing the translating state", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "load_state") return structuredClone(data);
      if (command === "stt_status")
        return { ready: true, modelName: "fixture", modelPath: "/tmp/model", sizeBytes: 1 };
      if (command === "queue_sentence_translations") return {};
      return undefined;
    });
    data.sessions[0]!.transcription = "Fresh.";
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Lesson");
    await wrapper.get('button[role="switch"]').trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain("翻译中");
    expect(wrapper.text()).not.toContain("翻译失败");

    const handler = listen.mock.calls.find(
      ([name]) => name === "translation-status",
    )![1];
    handler({
      payload: {
        sessionId: "s1",
        targetLanguage: "zh",
        sentences: ["Fresh."],
        translations: {},
        error: "provider unavailable",
      },
    });
    await nextTick();
    // The "translating…" indicator must not just vanish with nothing to show
    // for it: a failed batch surfaces its own status instead.
    expect(wrapper.text()).not.toContain("翻译中");
    expect(wrapper.text()).toContain("翻译失败");

    // A later successful batch for the same pair clears the error again.
    handler({
      payload: {
        sessionId: "s1",
        targetLanguage: "zh",
        sentences: ["Fresh."],
        translations: { "Fresh.": "全新翻译。" },
        error: null,
      },
    });
    await nextTick();
    expect(wrapper.text()).not.toContain("翻译失败");
    expect(wrapper.text()).toContain("全新翻译。");
    wrapper.unmount();
  });
});
