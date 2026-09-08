import { flushPromises, mount } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";
import App from "../src/App.vue";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke,
  Channel: class {
    onmessage?: (event: unknown) => void;
  },
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

const stubs = {
  FluentTheme: { template: "<div><slot /></div>" },
  FluentButton: { template: "<button v-bind='$attrs'><slot /></button>" },
  FluentNotice: { template: "<div><slot /></div>" },
  FluentDialog: { template: "<div><slot /><slot name='footer' /></div>" },
  FluentProgressBar: { template: "<div/>" },
  FluentSelect: { template: "<select/>" },
  FluentField: {
    props: ["modelValue", "label"],
    emits: ["update:modelValue"],
    template:
      "<label>{{label}}<input :value='modelValue' @input='$emit(\"update:modelValue\", $event.target.value)' /></label>",
  },
  FluentTextArea: {
    props: ["modelValue", "label"],
    emits: ["update:modelValue"],
    template:
      "<label>{{label}}<textarea :value='modelValue' @input='$emit(\"update:modelValue\", $event.target.value)' /></label>",
  },
  ModelConnections: { template: "<div/>" },
};

const state = () => ({
  workspaces: [{ id: "w1", name: "课程" }],
  sessions: [
    {
      id: "s1",
      workspaceId: "w1",
      title: "第一节",
      messages: [],
      transcription: "",
      summary: "",
    },
    {
      id: "s2",
      workspaceId: "w1",
      title: "第二节",
      messages: [],
      transcription: "",
      summary: "",
    },
  ],
  materials: [
    {
      id: "m1",
      workspaceId: "w1",
      name: "讲义.md",
      content: "内容",
      path: "/tmp/a.md",
    },
  ],
  settings: {
    providerId: "openai",
    model: "test",
    theme: "system",
    language: "zh",
  },
  providers: [
    { id: "openai", name: "OpenAI", baseUrl: "", models: [], hasKey: true },
  ],
});

describe("original workbench layout", () => {
  it("keeps session navigation, the knowledge tree and independent workbench panels", async () => {
    const data = state();
    invoke.mockImplementation((command: string) => {
      if (command === "load_state")
        return Promise.resolve(structuredClone(data));
      if (command === "stt_status")
        return Promise.resolve({
          ready: true,
          modelName: "local",
          modelPath: "",
          sizeBytes: 0,
        });
      return Promise.resolve();
    });
    const wrapper = mount(App, { global: { stubs } });
    await flushPromises();
    const sessions = wrapper.findAll(".tree-session");
    await sessions[1]!.trigger("click");
    expect(wrapper.find(".app-header h1").text()).toBe("第二节");
    expect(wrapper.find(".transcript-content").exists()).toBe(true);
    await wrapper
      .findAll("button")
      .find((item) => item.text() === "知识库")!
      .trigger("click");
    expect(wrapper.text()).toContain("讲义.md");
    await wrapper
      .findAll("button")
      .find((item) => item.text() === "摘要")!
      .trigger("click");
    expect(wrapper.find(".summary-page").exists()).toBe(true);
  });
});
