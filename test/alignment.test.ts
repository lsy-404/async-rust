import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";
import App from "../src/App.vue";
import type { AppData } from "../src/types";
import { open } from "@tauri-apps/plugin-dialog";
import { splitTranscriptSentences } from "../src/transcript-sentences";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke,
  Channel: class {
    onmessage?: (event: unknown) => void;
  },
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

// jsdom has no Blob URL implementation; the playback feature only needs a
// stable string handle back, not real object-URL semantics.
if (typeof URL.createObjectURL !== "function") {
  URL.createObjectURL = () => "blob:mock-audio";
}
if (typeof URL.revokeObjectURL !== "function") {
  URL.revokeObjectURL = () => undefined;
}

const stubs = {
  FluentTheme: { template: "<div><slot /></div>" },
  FluentButton: { template: "<button v-bind='$attrs'><slot /></button>" },
  FluentNotice: { template: "<div><slot /></div>" },
  FluentProgressBar: { template: "<div/>" },
  FluentDialog: {
    props: ["open", "label"],
    template:
      "<div v-if='open' class='dialog-stub' :aria-label='label'><div class='dialog-title'><slot name=\"title\" /></div><slot /><slot name='footer' /></div>",
  },
  FluentField: {
    props: ["modelValue", "label"],
    emits: ["update:modelValue"],
    template:
      "<label>{{ label }}<input :value='modelValue' @input='$emit(\"update:modelValue\", $event.target.value)' /></label>",
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
      "<label>{{ label }}<select :aria-label='label' :value='modelValue' @change='$emit(\"update:modelValue\", $event.target.value)'><option v-for='option in options' :value='option.value' :disabled='option.disabled'>{{ option.label }}</option></select></label>",
  },
  FluentSlider: {
    props: ["modelValue", "min", "max", "step", "label"],
    emits: ["update:modelValue"],
    template:
      "<input type='range' :min='min' :max='max' :step='step' :value='modelValue' @input='$emit(\"update:modelValue\", Number($event.target.value))' />",
  },
  ModelConnections: { template: "<div data-test='model-connections' />" },
};

function baseState(): AppData {
  return {
    workspaces: [
      { id: "w1", name: "Class A" },
      { id: "w2", name: "Class B" },
    ],
    sessions: [
      {
        id: "s1",
        workspaceId: "w1",
        title: "Lesson one",
        messages: [
          { id: "m1", role: "user", content: "Hello" },
          { id: "m2", role: "assistant", content: "Hi there" },
        ],
        transcription: "",
        summary: "",
      },
      {
        id: "s2",
        workspaceId: "w1",
        title: "Lesson two",
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
  };
}

function mockInvoke(data: AppData) {
  invoke.mockImplementation(
    async (command: string, args?: Record<string, any>) => {
      if (command === "load_state") return structuredClone(data);
      if (command === "stt_status")
        return {
          ready: true,
          modelName: "fixture",
          modelPath: "/tmp/model",
          sizeBytes: 1,
        };
      if (command === "save_settings")
        data.settings = args?.settings as AppData["settings"];
      if (command === "rename_workspace") {
        const workspace = data.workspaces.find((w) => w.id === args?.id);
        if (workspace) workspace.name = args?.name as string;
      }
      if (command === "rename_session") {
        const session = data.sessions.find((s) => s.id === args?.id);
        if (session) session.title = args?.title as string;
      }
      if (command === "save_session") {
        const index = data.sessions.findIndex(
          (s) => s.id === (args?.session as { id: string }).id,
        );
        if (index !== -1)
          data.sessions[index] = args?.session as AppData["sessions"][number];
      }
      return undefined;
    },
  );
}

function mountApp() {
  return mount(App, { global: { stubs } });
}

function buttonWithText(wrapper: VueWrapper, text: string) {
  const button = wrapper.findAll("button").find((item) => item.text() === text);
  if (!button) throw new Error(`Missing button: ${text}`);
  return button;
}

describe("sidebar tree behaviour", () => {
  let data: AppData;
  beforeEach(() => {
    data = baseState();
    invoke.mockReset();
    mockInvoke(data);
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("opens a context menu on right-click and closes it on Escape or outside click", async () => {
    const wrapper = mountApp();
    await flushPromises();
    const workspaceRow = wrapper.find(".workspace-node .tree-row");
    await workspaceRow.trigger("contextmenu");
    expect(wrapper.find(".context-menu").exists()).toBe(true);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    await nextTick();
    expect(wrapper.find(".context-menu").exists()).toBe(false);
    await workspaceRow.trigger("contextmenu");
    expect(wrapper.find(".context-menu").exists()).toBe(true);
    window.dispatchEvent(new MouseEvent("click"));
    await nextTick();
    expect(wrapper.find(".context-menu").exists()).toBe(false);
  });

  it("commits an inline session rename on Enter via rename_session, not save_session", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get(".tree-session").trigger("dblclick");
    const input = wrapper.get(".rename-input");
    await input.setValue("Renamed lesson");
    await input.trigger("keydown", { key: "Enter" });
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("rename_session", {
      id: "s1",
      title: "Renamed lesson",
    });
    expect(
      invoke.mock.calls.some(([command]) => command === "save_session"),
    ).toBe(false);
  });

  it("commits an inline workspace rename with the trimmed name via rename_workspace", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get(".tree-workspace").trigger("dblclick");
    const input = wrapper.get(".rename-input");
    await input.setValue("  Renamed class  ");
    await input.trigger("keydown", { key: "Enter" });
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("rename_workspace", {
      id: "w1",
      name: "Renamed class",
    });
  });

  it("cancels a rename on Escape and leaves the original name", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get(".tree-workspace").trigger("dblclick");
    const input = wrapper.get(".rename-input");
    await input.setValue("Should not stick");
    await input.trigger("keydown", { key: "Escape" });
    await flushPromises();
    expect(
      invoke.mock.calls.some(([command]) => command === "rename_workspace"),
    ).toBe(false);
    expect(wrapper.get(".tree-workspace").text()).toBe("Class A");
  });

  it("does not invoke anything for an empty or unchanged rename", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get(".tree-workspace").trigger("dblclick");
    let input = wrapper.get(".rename-input");
    await input.trigger("keydown", { key: "Enter" });
    await flushPromises();
    expect(
      invoke.mock.calls.some(([command]) => command === "rename_workspace"),
    ).toBe(false);

    await wrapper.get(".tree-workspace").trigger("dblclick");
    input = wrapper.get(".rename-input");
    await input.setValue("Class A");
    await input.trigger("keydown", { key: "Enter" });
    await flushPromises();
    expect(
      invoke.mock.calls.some(([command]) => command === "rename_workspace"),
    ).toBe(false);
  });

  it("collapses a workspace to hide its sessions", async () => {
    const wrapper = mountApp();
    await flushPromises();
    expect(wrapper.findAll(".tree-session").length).toBeGreaterThan(0);
    await wrapper.get(".tree-expand").trigger("click");
    expect(wrapper.findAll(".tree-session").length).toBe(0);
    await wrapper.get(".tree-expand").trigger("click");
    expect(wrapper.findAll(".tree-session").length).toBeGreaterThan(0);
  });

  it("shows a different empty state when a search term matches nothing versus no workspaces", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get(".sidebar-search input").setValue("nonexistent-term");
    await nextTick();
    expect(wrapper.find(".empty").text()).toBe("未找到匹配的工作区或会话");

    data.workspaces = [];
    data.sessions = [];
    const emptyWrapper = mountApp();
    await flushPromises();
    expect(emptyWrapper.find(".empty").text()).toBe("创建一个工作区开始整理课堂。");
  });
});

describe("dialog copy", () => {
  let data: AppData;
  beforeEach(() => {
    data = baseState();
    invoke.mockReset();
    mockInvoke(data);
  });

  it("renders a visible title for the new-session, settings and delete dialogs", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await buttonWithText(wrapper, "新建会话").trigger("click");
    expect(wrapper.find(".dialog-title").text()).toBe("新建会话");
    await buttonWithText(wrapper, "取消").trigger("click");

    await buttonWithText(wrapper, "设置").trigger("click");
    expect(wrapper.find(".dialog-title").text()).toBe("模型与外观");
    await buttonWithText(wrapper, "关闭").trigger("click");

    await wrapper.get(".tree-delete").trigger("click");
    expect(wrapper.find(".dialog-title").text()).toBe("删除工作区");
  });

  it("uses distinct delete copy for a workspace, a session and a material", async () => {
    data.materials = [
      {
        id: "mat1",
        workspaceId: "w1",
        name: "Notes.md",
        content: "content",
        path: "/tmp/notes.md",
      },
    ];
    const wrapper = mountApp();
    await flushPromises();
    const deleteButtons = wrapper.findAll(".tree-delete");
    await deleteButtons[0]!.trigger("click");
    expect(wrapper.find(".dialog-title").text()).toBe("删除工作区");
    expect(wrapper.text()).toContain('删除工作区"Class A"会同时删除其中的全部会话与材料');
    await buttonWithText(wrapper, "取消").trigger("click");

    const sessionDeleteButton = wrapper
      .findAll(".tree-delete")
      .find((_, index) => index > 0)!;
    await sessionDeleteButton.trigger("click");
    expect(wrapper.find(".dialog-title").text()).toBe("删除会话");
    expect(wrapper.text()).toContain('删除会话"Lesson one"后，其中的对话与转写记录将无法恢复');
  });

  it("passes the chosen workspace from the new-session dialog's picker to create_session", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await buttonWithText(wrapper, "新建会话").trigger("click");
    const selects = wrapper.findAll("select");
    const workspaceSelect = selects.find((select) =>
      select.findAll("option").some((option) => option.text() === "Class B"),
    )!;
    await workspaceSelect.setValue("w2");
    const titleLabel = wrapper
      .findAll("label")
      .find((label) => label.text().startsWith("会话标题"))!;
    await titleLabel.get("input").setValue("New topic");
    await buttonWithText(wrapper, "新建").trigger("click");
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("create_session", {
      workspaceId: "w2",
      title: "New topic",
    });
  });
});

describe("header controls", () => {
  let data: AppData;
  beforeEach(() => {
    data = baseState();
    invoke.mockReset();
    mockInvoke(data);
  });

  it("applies and persists a theme choice immediately without opening Settings", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await buttonWithText(wrapper, "深色").trigger("click");
    await flushPromises();
    expect(document.documentElement.dataset.fluentTheme).toBe("dark");
    expect(invoke).toHaveBeenCalledWith("save_settings", {
      settings: expect.objectContaining({ theme: "dark" }),
    });
    expect(data.settings.theme).toBe("dark");
    expect(wrapper.find(".dialog-stub").exists()).toBe(false);
  });

  it("swaps rendered copy to English and persists the language setting", async () => {
    const wrapper = mountApp();
    await flushPromises();
    expect(wrapper.text()).toContain("设置");
    await buttonWithText(wrapper, "English").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("Settings");
    expect(wrapper.text()).not.toContain("设置");
    expect(invoke).toHaveBeenCalledWith("save_settings", {
      settings: expect.objectContaining({ language: "en" }),
    });
    expect(data.settings.language).toBe("en");
  });

  it("marks the provider badge with the warning class when no provider is connected", async () => {
    data.providers[0]!.hasKey = false;
    const wrapper = mountApp();
    await flushPromises();
    const status = wrapper.get(".status");
    expect(status.classes()).toContain("status-warn");
    expect(status.text()).toBe("需要连接模型");
  });
});

describe("chat pane behaviour", () => {
  let data: AppData;
  beforeEach(() => {
    data = baseState();
    invoke.mockReset();
    mockInvoke(data);
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText: vi.fn().mockResolvedValue(undefined) },
      configurable: true,
    });
  });

  it("copies a message's content to the clipboard", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await buttonWithText(wrapper, "复制").trigger("click");
    await flushPromises();
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith("Hello");
  });

  it("persists the shortened message list via save_session when deleting a message", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await wrapper
      .findAll(".message-actions")[0]!
      .findAll("button")
      .find((item) => item.text() === "删除")!
      .trigger("click");
    await flushPromises();
    const call = invoke.mock.calls.find(([command]) => command === "save_session");
    expect(call).toBeTruthy();
    const savedSession = (call![1] as any).session;
    expect(savedSession.messages).toHaveLength(1);
    expect(savedSession.messages[0].id).toBe("m2");
  });

  it("drops the old assistant turn and re-invokes chat when regenerating", async () => {
    const savedSessionSnapshots: unknown[] = [];
    invoke.mockImplementation((command: string, args?: Record<string, any>) => {
      if (command === "load_state") return Promise.resolve(structuredClone(data));
      if (command === "stt_status")
        return Promise.resolve({
          ready: true,
          modelName: "fixture",
          modelPath: "/tmp",
          sizeBytes: 1,
        });
      if (command === "save_session") {
        savedSessionSnapshots.push(JSON.parse(JSON.stringify(args?.session)));
        const index = data.sessions.findIndex(
          (item) => item.id === args?.session.id,
        );
        if (index !== -1) data.sessions[index] = args?.session;
        return Promise.resolve();
      }
      if (command === "chat") {
        args?.onEvent.onmessage({ type: "delta", text: "new reply" });
        // Mimic the backend persisting the finished exchange, as it would by the time refresh() re-reads load_state.
        const target = data.sessions.find((item) => item.id === args?.sessionId)!;
        target.messages = [
          ...target.messages,
          { id: "regen-user", role: "user", content: args?.content },
          { id: "regen-assistant", role: "assistant", content: "new reply" },
        ];
        return Promise.resolve();
      }
      return Promise.resolve();
    });
    const wrapper = mountApp();
    await flushPromises();
    await wrapper
      .findAll(".message-actions")[1]!
      .findAll("button")
      .find((item) => item.text() === "重新生成")!
      .trigger("click");
    await flushPromises();
    expect(savedSessionSnapshots).toHaveLength(1);
    expect((savedSessionSnapshots[0] as any).messages).toHaveLength(0);
    const chatCall = invoke.mock.calls.find(([command]) => command === "chat");
    expect(chatCall![1]).toMatchObject({ sessionId: "s1", content: "Hello" });
    expect(wrapper.text()).not.toContain("Hi there");
    expect(wrapper.text()).toContain("new reply");
  });

  it("disables message actions while a stream is in flight", async () => {
    let releaseChat: () => void = () => undefined;
    invoke.mockImplementation((command: string, args?: Record<string, any>) => {
      if (command === "load_state") return Promise.resolve(structuredClone(data));
      if (command === "stt_status")
        return Promise.resolve({
          ready: true,
          modelName: "fixture",
          modelPath: "/tmp",
          sizeBytes: 1,
        });
      if (command === "chat")
        return new Promise<void>((resolve) => {
          releaseChat = resolve;
        });
      return Promise.resolve();
    });
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get("textarea").setValue("Another question");
    await wrapper.get("form.composer").trigger("submit");
    await nextTick();
    const actionButtons = wrapper
      .findAll(".message-actions")[0]!
      .findAll("button");
    for (const button of actionButtons) {
      expect(button.attributes("disabled")).toBeDefined();
    }
    releaseChat();
    await flushPromises();
  });

  it("auto-scrolls the chat when already at the bottom when a message is added", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "load_state") return Promise.resolve(structuredClone(data));
      if (command === "stt_status")
        return Promise.resolve({
          ready: true,
          modelName: "fixture",
          modelPath: "/tmp",
          sizeBytes: 1,
        });
      if (command === "chat") return new Promise(() => undefined);
      return Promise.resolve();
    });
    const wrapper = mountApp();
    await flushPromises();
    const panel = wrapper.get(".messages").element as HTMLElement;
    Object.defineProperty(panel, "scrollHeight", {
      value: 500,
      configurable: true,
    });
    Object.defineProperty(panel, "clientHeight", {
      value: 400,
      configurable: true,
    });
    panel.scrollTop = 100; // distanceFromBottom = 500-100-400 = 0 -> at bottom
    await wrapper.get("textarea").setValue("Another question");
    await wrapper.get("form.composer").trigger("submit");
    await nextTick();
    await flushPromises();
    await nextTick();
    expect(panel.scrollTop).toBe(500);
  });

  it("does not yank the view when the chat is scrolled up and a message is added", async () => {
    invoke.mockImplementation((command: string) => {
      if (command === "load_state") return Promise.resolve(structuredClone(data));
      if (command === "stt_status")
        return Promise.resolve({
          ready: true,
          modelName: "fixture",
          modelPath: "/tmp",
          sizeBytes: 1,
        });
      if (command === "chat") return new Promise(() => undefined);
      return Promise.resolve();
    });
    const wrapper = mountApp();
    await flushPromises();
    const panel = wrapper.get(".messages").element as HTMLElement;
    Object.defineProperty(panel, "scrollHeight", {
      value: 500,
      configurable: true,
    });
    Object.defineProperty(panel, "clientHeight", {
      value: 400,
      configurable: true,
    });
    panel.scrollTop = 50; // distanceFromBottom = 500-50-400 = 50 > 36 -> scrolled up
    await wrapper.get("textarea").setValue("Another question");
    await wrapper.get("form.composer").trigger("submit");
    await nextTick();
    await flushPromises();
    await nextTick();
    expect(panel.scrollTop).toBe(50);
  });
});

describe("summary and transcript panels", () => {
  let data: AppData;
  beforeEach(() => {
    data = baseState();
    invoke.mockReset();
    mockInvoke(data);
  });

  it("shows a formatted updated-at line only when summaryUpdatedAt is present", async () => {
    data.sessions[0]!.summaryUpdatedAt = "2024-03-05T10:00:00.000Z";
    data.sessions[0]!.summary = "Summary text";
    const wrapper = mountApp();
    await flushPromises();
    await buttonWithText(wrapper, "摘要").trigger("click");
    expect(wrapper.find(".summary-meta").exists()).toBe(true);
    expect(wrapper.find(".summary-meta").text()).toContain("更新时间：");

    data.sessions[0]!.summaryUpdatedAt = null;
    const noDateWrapper = mountApp();
    await flushPromises();
    await buttonWithText(noDateWrapper, "摘要").trigger("click");
    expect(noDateWrapper.find(".summary-meta").exists()).toBe(false);
  });

  it("remembers the chat/summary tab selection per session across a session switch", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await buttonWithText(wrapper, "摘要").trigger("click");
    expect(wrapper.find(".summary-page").exists()).toBe(true);
    await wrapper.findAll(".tree-session")[1]!.trigger("click");
    expect(wrapper.find(".summary-page").exists()).toBe(false);
    await wrapper.findAll(".tree-session")[0]!.trigger("click");
    expect(wrapper.find(".summary-page").exists()).toBe(true);
  });

  it("renders the workbench empty state with no chrome when no session is selected", async () => {
    data.sessions = [];
    const wrapper = mountApp();
    await flushPromises();
    expect(wrapper.find(".workbench-empty").exists()).toBe(true);
    expect(wrapper.find(".transcript-panel").exists()).toBe(false);
    expect(wrapper.find(".recording-bar").exists()).toBe(false);
  });
});

describe("settings persistence", () => {
  let data: AppData;
  beforeEach(() => {
    data = baseState();
    invoke.mockReset();
    mockInvoke(data);
  });

  it("round-trips the panel ratio and sidebar-open flag through save_settings", async () => {
    const wrapper = mountApp();
    await flushPromises();
    const layout = wrapper.get(".workbench-layout").element as HTMLElement;
    Object.defineProperty(layout, "clientWidth", {
      value: 900,
      configurable: true,
    });
    const handle = wrapper.get(".resize-handle");
    await handle.trigger("keydown", { key: "ArrowRight" });
    await flushPromises();
    let call = invoke.mock.calls
      .filter(([command]) => command === "save_settings")
      .pop()!;
    const expectedRatio = (Math.round(900 * 0.62) + 24) / 900;
    expect((call[1] as any).settings.mainPanelRatio).toBeCloseTo(expectedRatio, 5);
    expect((call[1] as any).settings.sidebarOpen).toBe(true);

    await wrapper.get('[aria-label="切换侧栏"]').trigger("click");
    await flushPromises();
    call = invoke.mock.calls
      .filter(([command]) => command === "save_settings")
      .pop()!;
    expect((call[1] as any).settings.sidebarOpen).toBe(false);
  });
});

// A stale context menu, rename box or message-edit box committed while a
// recording or chat stream is live would clobber data the backend is still writing.
describe("operationBusy guards on the new sidebar and message affordances", () => {
  let data: AppData;
  beforeEach(() => {
    data = baseState();
    invoke.mockReset();
  });

  function mockInvokeWithPendingChat() {
    invoke.mockImplementation((command: string, args?: Record<string, any>) => {
      if (command === "load_state") return Promise.resolve(structuredClone(data));
      if (command === "stt_status")
        return Promise.resolve({
          ready: true,
          modelName: "fixture",
          modelPath: "/tmp",
          sizeBytes: 1,
        });
      if (command === "save_settings")
        data.settings = args?.settings as AppData["settings"];
      if (command === "rename_workspace") {
        const workspace = data.workspaces.find((w) => w.id === args?.id);
        if (workspace) workspace.name = args?.name as string;
      }
      if (command === "rename_session") {
        const session = data.sessions.find((s) => s.id === args?.id);
        if (session) session.title = args?.title as string;
      }
      if (command === "chat") return new Promise(() => undefined); // never resolves: stream stays in flight
      return Promise.resolve();
    });
  }

  async function startStreaming(wrapper: VueWrapper) {
    await wrapper.get("form.composer textarea").setValue("Another question");
    await wrapper.get("form.composer").trigger("submit");
    await flushPromises();
  }

  it("refuses to open the context menu while a chat stream is in flight, and closes one already open", async () => {
    mockInvokeWithPendingChat();
    const wrapper = mountApp();
    await flushPromises();
    const row = wrapper.find(".workspace-node .tree-row");
    await row.trigger("contextmenu");
    expect(wrapper.find(".context-menu").exists()).toBe(true);

    await startStreaming(wrapper);
    // The stream now holds the generation slot; a menu left open over it must
    // not stay open for the user to act on with a now-busy session.
    expect(wrapper.find(".context-menu").exists()).toBe(false);
    await row.trigger("contextmenu");
    expect(wrapper.find(".context-menu").exists()).toBe(false);
  });

  it("closes an open session rename box and never commits it once a chat stream starts", async () => {
    mockInvokeWithPendingChat();
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get(".tree-session").trigger("dblclick");
    expect(wrapper.find(".rename-input").exists()).toBe(true);

    await startStreaming(wrapper);
    expect(wrapper.find(".rename-input").exists()).toBe(false);
    expect(
      invoke.mock.calls.some(([command]) => command === "rename_session"),
    ).toBe(false);
    expect(
      invoke.mock.calls.some(([command]) => command === "save_session"),
    ).toBe(false);
    // The session title in the tree must still read the un-renamed value.
    expect(wrapper.get(".tree-session").text()).toContain("Lesson one");
  });

  it("closes an open message-edit box and never saves it once a chat stream starts", async () => {
    mockInvokeWithPendingChat();
    const wrapper = mountApp();
    await flushPromises();
    await wrapper
      .findAll(".message-actions")[0]!
      .findAll("button")
      .find((item) => item.text() === "编辑")!
      .trigger("click");
    expect(wrapper.find(".message-edit").exists()).toBe(true);
    await wrapper.get(".message-edit textarea").setValue("Edited content");

    await startStreaming(wrapper);
    expect(wrapper.find(".message-edit").exists()).toBe(false);
    expect(
      invoke.mock.calls.some(([command]) => command === "save_session"),
    ).toBe(false);
    expect(wrapper.text()).not.toContain("Edited content");
  });

  it("disables the delete-confirmation button once an operation starts, without closing the dialog under it", async () => {
    mockInvokeWithPendingChat();
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get(".tree-delete").trigger("click");
    expect(wrapper.find(".dialog-stub").exists()).toBe(true);
    expect(
      buttonWithText(wrapper, "删除工作区").attributes("disabled"),
    ).toBeUndefined();

    await startStreaming(wrapper);
    // Unlike the context menu and edit box, the delete dialog itself stays
    // open -- only the destructive action inside it is locked out.
    expect(wrapper.find(".dialog-stub").exists()).toBe(true);
    expect(
      buttonWithText(wrapper, "删除工作区").attributes("disabled"),
    ).toBeDefined();
  });

  it("marks the session busy before the regenerate save awaits, so a second send is refused in the gap", async () => {
    let releaseSave: () => void = () => undefined;
    invoke.mockImplementation((command: string, args?: Record<string, any>) => {
      if (command === "load_state") return Promise.resolve(structuredClone(data));
      if (command === "stt_status")
        return Promise.resolve({
          ready: true,
          modelName: "fixture",
          modelPath: "/tmp",
          sizeBytes: 1,
        });
      if (command === "save_session")
        return new Promise<void>((resolve) => {
          releaseSave = resolve;
        });
      return Promise.resolve();
    });
    const wrapper = mountApp();
    await flushPromises();
    await wrapper
      .findAll(".message-actions")[1]!
      .findAll("button")
      .find((item) => item.text() === "重新生成")!
      .trigger("click");
    await nextTick();
    // regenerateMessage is now paused awaiting save_session; streaming must
    // already be true so a second send cannot start a concurrent generation.
    await wrapper.get("form.composer textarea").setValue("Another question");
    await wrapper.get("form.composer").trigger("submit");
    await flushPromises();
    expect(
      invoke.mock.calls.some(([command]) => command === "chat"),
    ).toBe(false);
    releaseSave();
    await flushPromises();
  });
});

describe("material editor draft", () => {
  let data: AppData;
  beforeEach(() => {
    data = baseState();
    data.sessions = [];
    data.materials = [
      {
        id: "mat1",
        workspaceId: "w1",
        name: "Notes.md",
        content: "original content",
        path: "/tmp/notes.md",
      },
      {
        id: "mat2",
        workspaceId: "w1",
        name: "Other.md",
        content: "other content",
        path: "/tmp/other.md",
      },
    ];
    invoke.mockReset();
    mockInvoke(data);
  });

  it("keeps an unsaved draft across an unrelated refresh, but swaps it when a different material is selected", async () => {
    const wrapper = mountApp();
    await flushPromises();
    expect(
      (wrapper.get(".material-editor textarea").element as HTMLTextAreaElement)
        .value,
    ).toBe("original content");
    await wrapper.get(".material-editor textarea").setValue("unsaved edits");

    // Rename the workspace: an action wholly unrelated to the open material,
    // but one that still triggers a refresh() and a fresh load_state payload.
    await wrapper.get(".tree-workspace").trigger("dblclick");
    const input = wrapper.get(".rename-input");
    await input.setValue("Renamed class");
    await input.trigger("keydown", { key: "Enter" });
    await flushPromises();
    expect(
      invoke.mock.calls.some(([command]) => command === "rename_workspace"),
    ).toBe(true);
    expect(
      (wrapper.get(".material-editor textarea").element as HTMLTextAreaElement)
        .value,
    ).toBe("unsaved edits");

    // Selecting a different material still swaps the draft as expected.
    await buttonWithText(wrapper, "知识库").trigger("click");
    const materialRows = wrapper.findAll(".tree-session");
    await materialRows[1]!.trigger("click");
    await nextTick();
    expect(
      (wrapper.get(".material-editor textarea").element as HTMLTextAreaElement)
        .value,
    ).toBe("other content");
  });
});

describe("settings dialog theme control", () => {
  let data: AppData;
  beforeEach(() => {
    data = baseState();
    invoke.mockReset();
    mockInvoke(data);
  });

  it("has no theme control of its own in the Settings dialog; the header switcher is the sole, immediate theme control", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await buttonWithText(wrapper, "设置").trigger("click");
    const dialog = wrapper.get(".dialog-stub");
    expect(dialog.findAll("select").length).toBe(0);
    expect(dialog.text()).not.toContain("主题");
    await buttonWithText(wrapper, "关闭").trigger("click");

    await buttonWithText(wrapper, "深色").trigger("click");
    await flushPromises();
    expect(document.documentElement.dataset.fluentTheme).toBe("dark");
    expect(data.settings.theme).toBe("dark");
  });
});

describe("chat gap fixes: edit, inline error, uncertainty markup, IME safety", () => {
  let data: AppData;
  beforeEach(() => {
    data = baseState();
    invoke.mockReset();
    mockInvoke(data);
  });

  it("truncates from the edited turn and re-runs chat instead of leaving the stale reply below it", async () => {
    const savedSessionSnapshots: unknown[] = [];
    invoke.mockImplementation((command: string, args?: Record<string, any>) => {
      if (command === "load_state") return Promise.resolve(structuredClone(data));
      if (command === "stt_status")
        return Promise.resolve({
          ready: true,
          modelName: "fixture",
          modelPath: "/tmp",
          sizeBytes: 1,
        });
      if (command === "save_session") {
        savedSessionSnapshots.push(JSON.parse(JSON.stringify(args?.session)));
        const index = data.sessions.findIndex(
          (item) => item.id === args?.session.id,
        );
        if (index !== -1) data.sessions[index] = args?.session;
        return Promise.resolve();
      }
      if (command === "chat") {
        args?.onEvent.onmessage({ type: "delta", text: "revised reply" });
        const target = data.sessions.find((item) => item.id === args?.sessionId)!;
        target.messages = [
          ...target.messages,
          { id: "edit-user", role: "user", content: args?.content },
          { id: "edit-assistant", role: "assistant", content: "revised reply" },
        ];
        return Promise.resolve();
      }
      return Promise.resolve();
    });
    const wrapper = mountApp();
    await flushPromises();
    await wrapper
      .findAll(".message-actions")[0]!
      .findAll("button")
      .find((item) => item.text() === "编辑")!
      .trigger("click");
    await wrapper.get(".message-edit textarea").setValue("Revised question");
    await wrapper
      .get(".message-edit")
      .findAll("button")
      .find((item) => item.text() === "保存")!
      .trigger("click");
    await flushPromises();
    expect(savedSessionSnapshots).toHaveLength(1);
    expect((savedSessionSnapshots[0] as any).messages).toHaveLength(0);
    const chatCall = invoke.mock.calls.find(([command]) => command === "chat");
    expect(chatCall![1]).toMatchObject({
      sessionId: "s1",
      content: "Revised question",
    });
    // The stale assistant reply to the original question must be gone.
    expect(wrapper.text()).not.toContain("Hi there");
    expect(wrapper.text()).toContain("revised reply");
  });

  it("renders a failed generation inline on the assistant turn instead of dropping it", async () => {
    let rejectChat: (cause: Error) => void = () => undefined;
    invoke.mockImplementation((command: string) => {
      if (command === "load_state") return Promise.resolve(structuredClone(data));
      if (command === "stt_status")
        return Promise.resolve({
          ready: true,
          modelName: "fixture",
          modelPath: "/tmp",
          sizeBytes: 1,
        });
      if (command === "chat")
        return new Promise((_, reject) => {
          rejectChat = reject;
        });
      return Promise.resolve();
    });
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get("form.composer textarea").setValue("Will this fail?");
    await wrapper.get("form.composer").trigger("submit");
    await nextTick();
    rejectChat(new Error("network unavailable"));
    await flushPromises();
    const messages = wrapper.findAll(".message");
    const lastMessage = messages[messages.length - 1]!;
    expect(lastMessage.classes()).toContain("assistant");
    expect(lastMessage.text()).toContain("network unavailable");
  });

  it("highlights an uncertain <maybe> span and its follow-up action resends the exact claim to verify", async () => {
    data.sessions[0]!.messages.push({
      id: "m3",
      role: "assistant",
      content: "The lecture is <maybe>scheduled for 3pm</maybe> tomorrow.",
    });
    const wrapper = mountApp();
    await flushPromises();
    const span = wrapper.get(".uncertain-span");
    expect(span.text()).toBe("scheduled for 3pm");

    await span.trigger("mousemove");
    await nextTick();
    const tooltip = wrapper.get(".uncertain-tooltip");
    await tooltip
      .findAll("button")
      .find((item) => item.text() === "追问核实")!
      .trigger("click");
    await flushPromises();
    const chatCall = invoke.mock.calls.find(([command]) => command === "chat");
    expect(chatCall).toBeTruthy();
    expect(chatCall![1].content).toContain("scheduled for 3pm");
  });

  it("ignores Enter during IME composition even without isComposing on the event, and sends once composition ends", async () => {
    const wrapper = mountApp();
    await flushPromises();
    const textarea = wrapper.get("form.composer textarea");
    await textarea.setValue("こんにちは");
    await textarea.trigger("compositionstart");
    await textarea.trigger("keydown", { key: "Enter" });
    await flushPromises();
    expect(
      invoke.mock.calls.some(([command]) => command === "chat"),
    ).toBe(false);

    await textarea.trigger("compositionend");
    await textarea.trigger("keydown", { key: "Enter" });
    await flushPromises();
    expect(
      invoke.mock.calls.some(([command]) => command === "chat"),
    ).toBe(true);
  });

  it("also treats keyCode 229 alone as composition, for browsers that never set isComposing", async () => {
    const wrapper = mountApp();
    await flushPromises();
    const textarea = wrapper.get("form.composer textarea");
    await textarea.setValue("中文输入法");
    await textarea.trigger("keydown", { key: "Enter", keyCode: 229 });
    await flushPromises();
    expect(
      invoke.mock.calls.some(([command]) => command === "chat"),
    ).toBe(false);
  });
});

describe("transcript sentence segmentation (pure function)", () => {
  it("splits on Chinese and English sentence punctuation", () => {
    expect(splitTranscriptSentences("第一句。第二句！第三句？")).toEqual([
      "第一句。",
      "第二句！",
      "第三句？",
    ]);
    expect(
      splitTranscriptSentences("First sentence. Second sentence! Really?"),
    ).toEqual(["First sentence.", "Second sentence!", "Really?"]);
  });

  it("keeps a trailing fragment with no terminal punctuation as its own sentence", () => {
    expect(splitTranscriptSentences("完整的句子。还在说的半句")).toEqual([
      "完整的句子。",
      "还在说的半句",
    ]);
  });

  it("returns an empty list for blank input", () => {
    expect(splitTranscriptSentences("")).toEqual([]);
    expect(splitTranscriptSentences("   \n  ")).toEqual([]);
  });
});

describe("transcript gap fixes: capture mode, language, level meter, playback", () => {
  let data: AppData;
  beforeEach(() => {
    data = baseState();
    invoke.mockReset();
    mockInvoke(data);
    vi.mocked(open).mockReset();
  });

  it("renders the transcript as separate sentence items instead of one block", async () => {
    data.sessions[0]!.transcription = "第一句。第二句！Third sentence.";
    const wrapper = mountApp();
    await flushPromises();
    const sentences = wrapper.findAll(".transcript-sentence");
    expect(sentences.map((item) => item.text())).toEqual([
      "第一句。",
      "第二句！",
      "Third sentence.",
    ]);
  });

  it("unifies capture behind one mode selector and one action button", async () => {
    const wrapper = mountApp();
    await flushPromises();
    expect(buttonWithText(wrapper, "开始录音").exists()).toBe(true);
    expect(wrapper.findAll("button").some((b) => b.text() === "导入音频")).toBe(
      false,
    );

    const modeSelect = wrapper.get('[aria-label="输入方式"]');
    await modeSelect.setValue("upload");
    await nextTick();
    expect(buttonWithText(wrapper, "导入音频").exists()).toBe(true);
    expect(
      wrapper.findAll("button").some((b) => b.text() === "开始录音"),
    ).toBe(false);

    vi.mocked(open).mockResolvedValue("/tmp/lecture.wav");
    await buttonWithText(wrapper, "导入音频").trigger("click");
    await flushPromises();
    expect(open).toHaveBeenCalled();
    expect(invoke).toHaveBeenCalledWith(
      "transcribe_audio",
      expect.objectContaining({ sessionId: "s1", path: "/tmp/lecture.wav" }),
    );
  });

  it("renders the system-audio option disabled with a short explanation instead of omitting or faking it", async () => {
    const wrapper = mountApp();
    await flushPromises();
    const modeSelect = wrapper.get('[aria-label="输入方式"]');
    const systemOption = modeSelect
      .findAll("option")
      .find((option) => option.element.value === "system")!;
    expect(systemOption.attributes("disabled")).toBeDefined();
    expect(systemOption.text()).toContain("暂不可用");
  });

  it("passes the picked transcription language into transcribe_audio", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get('[aria-label="转写语言"]').setValue("ja");
    await wrapper.get('[aria-label="输入方式"]').setValue("upload");
    vi.mocked(open).mockResolvedValue("/tmp/lecture.wav");
    await buttonWithText(wrapper, "导入音频").trigger("click");
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("transcribe_audio", {
      sessionId: "s1",
      path: "/tmp/lecture.wav",
      language: "ja",
    });
  });

  it("passes the picked transcription language into start_recording, and null when left on auto-detect", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get('[aria-label="转写语言"]').setValue("fr");
    await buttonWithText(wrapper, "开始录音").trigger("click");
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith(
      "start_recording",
      expect.objectContaining({ sessionId: "s1", language: "fr" }),
    );
  });

  it("renders a live level meter while recording and resets it once recording stops", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await buttonWithText(wrapper, "开始录音").trigger("click");
    await flushPromises();
    expect(wrapper.find(".level-meter").exists()).toBe(true);
    const start = invoke.mock.calls.find(
      ([command]) => command === "start_recording",
    )!;
    const channel = start[1].onEvent;
    channel.onmessage({ type: "level", sessionId: "s1", level: 0.8 });
    await nextTick();
    const bars = wrapper.findAll(".level-meter span");
    expect(bars[bars.length - 1]!.attributes("style")).toContain("16.8px");

    await buttonWithText(wrapper, "停止录音").trigger("click");
    await flushPromises();
    expect(wrapper.find(".level-meter").exists()).toBe(false);

    await buttonWithText(wrapper, "开始录音").trigger("click");
    await flushPromises();
    const restartedBars = wrapper.findAll(".level-meter span");
    expect(restartedBars[restartedBars.length - 1]!.attributes("style")).toContain(
      "4px",
    );
  });

  it("offers play/pause and a seek slider for an imported file, and revokes the blob URL on session switch", async () => {
    vi.mocked(open).mockResolvedValue("/tmp/lecture.wav");
    const revokeSpy = vi.spyOn(URL, "revokeObjectURL");
    const wrapper = mountApp();
    await flushPromises();
    expect(wrapper.find(".playback-bar").exists()).toBe(false);

    await wrapper.get('[aria-label="输入方式"]').setValue("upload");
    await buttonWithText(wrapper, "导入音频").trigger("click");
    await flushPromises();

    const playbackBar = wrapper.get(".playback-bar");
    const playButton = buttonWithText(wrapper, "播放");
    const audio = playbackBar.get("audio").element as HTMLAudioElement;
    await playButton.trigger("click");
    await audio.dispatchEvent(new Event("play"));
    await nextTick();
    expect(buttonWithText(wrapper, "暂停").exists()).toBe(true);

    // jsdom never fires real media events; give the slider a real range to
    // seek within by reporting duration the same way a browser would.
    Object.defineProperty(audio, "duration", { value: 30, configurable: true });
    await audio.dispatchEvent(new Event("loadedmetadata"));
    await nextTick();
    const slider = playbackBar.get("input[type=range]");
    await slider.setValue(12);
    await nextTick();
    expect(audio.currentTime).toBe(12);

    await wrapper.findAll(".tree-session")[1]!.trigger("click");
    await flushPromises();
    expect(revokeSpy).toHaveBeenCalled();
    expect(wrapper.find(".playback-bar").exists()).toBe(false);
  });
});
