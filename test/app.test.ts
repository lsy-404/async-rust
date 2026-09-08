import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";
import App from "../src/App.vue";
import type { AppData } from "../src/types";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke,
  Channel: class {
    onmessage?: (event: unknown) => void;
  },
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("@model-auth/vue", async () => {
  const { defineComponent, h } = await import("vue");
  return {
    ModelAuthDialog: defineComponent({
      props: ["providers"],
      emits: ["remove-api-key"],
      setup(props, { emit }) {
        return () =>
          h(
            "button",
            {
              "data-test": "remove-key",
              onClick: () => emit("remove-api-key", props.providers?.[0]?.id),
            },
            "remove",
          );
      },
    }),
  };
});

const state = (): AppData => ({
  workspaces: [
    { id: "w1", name: "Class A" },
    { id: "w2", name: "Class B" },
  ],
  sessions: [
    {
      id: "s1",
      workspaceId: "w1",
      title: "Lesson",
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
  FluentField: {
    props: ["modelValue", "label"],
    emits: ["update:modelValue"],
    template:
      "<label>{{ label }}<input :aria-label='label' :value='modelValue' @input='$emit(\"update:modelValue\", $event.target.value)' /></label>",
  },
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
  ModelAuthDialog: {
    props: ["providers"],
    emits: ["remove-api-key"],
    template:
      "<button data-test='remove-key' @click='$emit(\"remove-api-key\", providers[0].id)'>remove</button>",
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

describe("desktop workbench interactions", () => {
  let data: AppData;
  beforeEach(() => {
    data = state();
    invoke.mockReset();
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
        if (command === "save_provider") {
          const provider = args?.provider as AppData["providers"][number];
          const index = data.providers.findIndex(
            (item) => item.id === provider.id,
          );
          const saved = {
            ...provider,
            hasKey: args?.apiKey === "" ? false : provider.hasKey,
          };
          if (index < 0) data.providers.push(saved);
          else data.providers[index] = saved;
        }
        if (command === "save_settings")
          data.settings = args?.settings as AppData["settings"];
        return undefined;
      },
    );
    vi.stubGlobal("crypto", { randomUUID: () => "new-provider" });
  });

  it("persists a new provider selection across the refresh after creation", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get('input[aria-label="新增供应商名称"]').setValue("Fixture");
    await wrapper
      .get('input[aria-label="新增供应商 API 地址"]')
      .setValue("http://fixture/v1");
    await buttonWithText(wrapper, "新增供应商").trigger("click");
    await flushPromises();
    expect(data.settings.providerId).toBe("new-provider");
    expect(
      (
        wrapper.get('select[aria-label="模型供应商"]')
          .element as HTMLSelectElement
      ).value,
    ).toBe("new-provider");
    expect(wrapper.text()).toContain("TXT、Markdown 或 DOCX");
    expect(wrapper.text()).not.toContain("导入 PDF");
  });

  it("removes a key through model-auth without deleting its provider", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get('[data-test="remove-key"]').trigger("click");
    await flushPromises();
    expect(data.providers).toHaveLength(1);
    expect(data.providers[0]?.hasKey).toBe(false);
    expect(invoke).toHaveBeenCalledWith("save_provider", {
      provider: expect.objectContaining({ id: "openai" }),
      apiKey: "",
    });
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
    await wrapper.get("textarea").setValue("Explain this");
    await wrapper.get("form.composer").trigger("submit");
    await nextTick();
    expect(wrapper.text()).toContain("partial response");
    rejectChat(new Error("fixture unavailable"));
    await flushPromises();
    expect(wrapper.text()).toContain("Explain this");
    expect(wrapper.text()).toContain("fixture unavailable");
  });

  it("locks workspace navigation while microphone capture is active", async () => {
    const stream = {
      getTracks: () => [{ stop: vi.fn() }],
    } as unknown as MediaStream;
    vi.stubGlobal(
      "AudioContext",
      class {
        sampleRate = 48_000;
        destination = {};
        createMediaStreamSource() {
          return { connect: vi.fn(), disconnect: vi.fn() };
        }
        createScriptProcessor() {
          return {
            connect: vi.fn(),
            disconnect: vi.fn(),
            onaudioprocess: undefined,
          };
        }
        close() {
          return Promise.resolve();
        }
      },
    );
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: { getUserMedia: vi.fn().mockResolvedValue(stream) },
    });
    const wrapper = mountApp();
    await flushPromises();
    await buttonWithText(wrapper, "录音转写").trigger("click");
    await flushPromises();
    const rows = wrapper.findAll("button.nav-row");
    expect(rows[1]?.attributes("disabled")).toBeDefined();
  });
});
