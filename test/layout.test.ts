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
  FluentSelect: {
    props: ["modelValue", "options"],
    emits: ["update:modelValue"],
    template:
      "<select :value='modelValue' @change='$emit(\"update:modelValue\", $event.target.value)'><option v-for='option in options' :value='option.value'>{{option.label}}</option></select>",
  },
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

// Mixed tree: two folders, each holding a session and a folder holds a
// material too, matching the node-tree owner decision (sessions and
// materials live at root or in any folder, in one tree).
const state = () => ({
  nodes: [
    { id: "w1", parentId: null, kind: "folder", name: "课程" },
    { id: "w2", parentId: null, kind: "folder", name: "研讨" },
    { id: "s1", parentId: "w1", kind: "session", name: "第一节" },
    { id: "s2", parentId: "w1", kind: "session", name: "第二节" },
    { id: "s3", parentId: "w2", kind: "session", name: "第三节" },
    { id: "m1", parentId: "w1", kind: "material", name: "讲义.md" },
    { id: "m2", parentId: "w2", kind: "material", name: "研讨记录.txt" },
  ],
  sessions: [
    { id: "s1", messages: [], transcription: "", summary: "" },
    { id: "s2", messages: [], transcription: "", summary: "" },
    { id: "s3", messages: [], transcription: "", summary: "" },
  ],
  materials: [
    { id: "m1", content: "内容", path: "/tmp/a.md" },
    { id: "m2", content: "内容", path: "/tmp/b.txt" },
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

function findByText(wrapper: ReturnType<typeof mount>, selector: string, text: string) {
  const match = wrapper.findAll(selector).find((item) => item.text() === text);
  if (!match) throw new Error(`Missing ${selector} with text: ${text}`);
  return match;
}
async function expandFolder(wrapper: ReturnType<typeof mount>, name: string) {
  await findByText(wrapper, ".tree-folder", name).trigger("click");
}

describe("original workbench layout", () => {
  it("keeps session and material navigation as independent workbench panels", async () => {
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
    await expandFolder(wrapper, "课程");
    await expandFolder(wrapper, "研讨");
    await findByText(wrapper, ".tree-session", "第三节").trigger("click");
    expect(wrapper.find(".app-header h1").text()).toBe("第三节");
    expect(wrapper.find(".transcript-content").exists()).toBe(true);
    await wrapper.get('[aria-label="切换侧栏"]').trigger("click");
    expect(wrapper.find(".explorer").exists()).toBe(false);
    await wrapper.get('[aria-label="切换侧栏"]').trigger("click");
    expect(wrapper.find(".explorer").exists()).toBe(true);

    // Selecting a material opens it independently of the session workbench.
    await findByText(wrapper, ".tree-material", "讲义.md").trigger("click");
    expect(wrapper.text()).toContain("讲义.md");
    expect(wrapper.find(".material-editor").exists()).toBe(true);
    expect(wrapper.find(".transcript-content").exists()).toBe(false);

    // Reopening the session keeps the chat/summary tabs and resize handle.
    await findByText(wrapper, ".tree-session", "第三节").trigger("click");
    await findByText(wrapper, "button", "摘要").trigger("click");
    expect(wrapper.find(".summary-page").exists()).toBe(true);
    expect(wrapper.find(".resize-handle").attributes("aria-valuenow")).toBe(
      "62",
    );
  });

  it("locks navigation to a different session after native recording starts", async () => {
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
      if (command === "start_recording") return Promise.resolve();
      return Promise.resolve();
    });
    const wrapper = mount(App, { global: { stubs } });
    await flushPromises();
    await expandFolder(wrapper, "课程");
    await findByText(wrapper, ".tree-session", "第一节").trigger("click");
    await findByText(wrapper, "button", "开始录音").trigger("click");
    await flushPromises();
    expect(
      findByText(wrapper, ".tree-session", "第二节").attributes("disabled"),
    ).toBeDefined();
    // Expand/collapse stays allowed while busy.
    await expandFolder(wrapper, "研讨");
    expect(wrapper.find(".explorer").text()).toContain("第三节");
  });
});
