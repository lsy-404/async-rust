import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
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

const state = (): AppData => ({
  workspaces: [{ id: "w1", name: "Class A" }],
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

describe("tool call cards in the assistant turn", () => {
  let data: AppData;
  beforeEach(() => {
    data = state();
    invoke.mockReset();
  });

  it("walks a tool call through requested, running and finished, survives the post-stream reload, and shows args/result once expanded", async () => {
    invoke.mockImplementation((command: string, args?: Record<string, any>) => {
      if (command === "load_state") return Promise.resolve(structuredClone(data));
      if (command === "stt_status")
        return Promise.resolve({
          ready: true,
          modelName: "fixture",
          modelPath: "/tmp",
          sizeBytes: 1,
        });
      if (command === "chat") {
        args?.onEvent.onmessage({
          type: "tool",
          text: "",
          toolCallId: "call_1",
          toolName: "search_local_materials",
          toolStatus: "requested",
          toolArguments: '{"query":"algebra"}',
        });
        args?.onEvent.onmessage({
          type: "tool",
          text: "",
          toolCallId: "call_1",
          toolName: "search_local_materials",
          toolStatus: "running",
          toolArguments: '{"query":"algebra"}',
        });
        args?.onEvent.onmessage({
          type: "tool",
          text: "",
          toolCallId: "call_1",
          toolName: "search_local_materials",
          toolStatus: "finished",
          toolArguments: '{"query":"algebra"}',
          toolResult: "[Material] notes.txt: algebra basics",
        });
        args?.onEvent.onmessage({ type: "delta", text: "Here is what I found." });
        // Mirrors what a persisting backend now writes: the assistant turn's
        // load_state row carries toolCalls forward, not just the live stream.
        data.sessions[0].messages.push(
          { id: "u1", role: "user", content: "What is algebra?" },
          {
            id: "m1",
            role: "assistant",
            content: "Here is what I found.",
            toolCalls: [
              {
                id: "call_1",
                name: "search_local_materials",
                status: "finished",
                arguments: '{"query":"algebra"}',
                result: "[Material] notes.txt: algebra basics",
              },
            ],
          },
        );
        return Promise.resolve(undefined);
      }
      return Promise.resolve(undefined);
    });
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get("textarea").setValue("What is algebra?");
    await wrapper.get("form.composer").trigger("submit");
    await flushPromises();
    // The turn has finished and streamAssistantReply's refresh() has already
    // replaced data.value wholesale from load_state by this point - so this
    // is asserting against the reloaded message, not the live streaming one.
    expect(wrapper.text()).toContain("search_local_materials");
    expect(wrapper.text()).toContain("已完成");
    expect(wrapper.text()).not.toContain("algebra basics");
    await wrapper.get(".tool-call-header").trigger("click");
    expect(wrapper.text()).toContain('"query": "algebra"');
    expect(wrapper.text()).toContain("algebra basics");
    expect(wrapper.text()).toContain("Here is what I found.");
  });

  it("renders a failed tool call distinctly and still completes the turn", async () => {
    invoke.mockImplementation((command: string, args?: Record<string, any>) => {
      if (command === "load_state") return Promise.resolve(structuredClone(data));
      if (command === "stt_status")
        return Promise.resolve({
          ready: true,
          modelName: "fixture",
          modelPath: "/tmp",
          sizeBytes: 1,
        });
      if (command === "chat") {
        args?.onEvent.onmessage({
          type: "tool",
          text: "",
          toolCallId: "call_2",
          toolName: "search_local_materials",
          toolStatus: "requested",
          toolArguments: "{}",
        });
        args?.onEvent.onmessage({
          type: "tool",
          text: "",
          toolCallId: "call_2",
          toolName: "search_local_materials",
          toolStatus: "failed",
          toolArguments: "{}",
          toolResult: "查询内容不能为空。",
        });
        args?.onEvent.onmessage({ type: "delta", text: "I could not search." });
        data.sessions[0].messages.push(
          { id: "u2", role: "user", content: "Search nothing" },
          {
            id: "m2",
            role: "assistant",
            content: "I could not search.",
            toolCalls: [
              {
                id: "call_2",
                name: "search_local_materials",
                status: "failed",
                arguments: "{}",
                result: "查询内容不能为空。",
              },
            ],
          },
        );
        return Promise.resolve(undefined);
      }
      return Promise.resolve(undefined);
    });
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get("textarea").setValue("Search nothing");
    await wrapper.get("form.composer").trigger("submit");
    await flushPromises();
    // Still true after streamAssistantReply's post-stream refresh() has
    // replaced data.value wholesale from the (mocked) persisted state.
    expect(wrapper.find(".tool-call-failed").exists()).toBe(true);
    expect(wrapper.text()).toContain("失败");
    expect(wrapper.text()).toContain("I could not search.");
  });
});
