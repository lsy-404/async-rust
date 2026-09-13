import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";
import App from "../src/App.vue";
import type { AppData } from "../src/types";
import { open } from "@tauri-apps/plugin-dialog";
import { splitTranscriptSentences } from "../src/transcript-sentences";
import { subtreeIds } from "../src/explorer/tree";

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
// jsdom has no layout engine, so it does not implement scrollIntoView at all.
if (typeof Element.prototype.scrollIntoView !== "function") {
  Element.prototype.scrollIntoView = () => undefined;
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
    nodes: [
      { id: "s1", parentId: null, kind: "session", name: "Lesson one" },
      { id: "s2", parentId: null, kind: "session", name: "Lesson two" },
    ],
    sessions: [
      {
        id: "s1",
        messages: [
          { id: "m1", role: "user", content: "Hello" },
          { id: "m2", role: "assistant", content: "Hi there" },
        ],
        transcription: "",
        summary: "",
      },
      {
        id: "s2",
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
      if (command === "save_messages") {
        const session = data.sessions.find((s) => s.id === args?.sessionId);
        if (session)
          session.messages = args?.messages as AppData["sessions"][number]["messages"];
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
async function openSessionNode(wrapper: VueWrapper, name: string) {
  const button = wrapper
    .findAll(".tree-session")
    .find((item) => item.text() === name);
  if (!button) throw new Error(`Missing session node: ${name}`);
  await button.trigger("click");
  await flushPromises();
}

// Context menus, inline rename, create and delete are not wired to the tree
// yet (a later step); this only covers rendering, expand/collapse and
// selection against the node tree.
describe("explorer tree behaviour", () => {
  let data: AppData;
  beforeEach(() => {
    data = baseState();
    invoke.mockReset();
    mockInvoke(data);
  });

  it("selecting a session in the tree opens it exactly as before", async () => {
    const wrapper = mountApp();
    await flushPromises();
    expect(wrapper.find(".workbench-empty").exists()).toBe(true);
    await openSessionNode(wrapper, "Lesson one");
    expect(wrapper.find(".app-header h1").text()).toBe("Lesson one");
    expect(wrapper.find(".transcript-content").exists()).toBe(true);
  });

  it("collapses a folder to hide its children", async () => {
    data.nodes = [
      { id: "w1", parentId: null, kind: "folder", name: "Class A" },
      { id: "s1", parentId: "w1", kind: "session", name: "Lesson one" },
    ];
    const wrapper = mountApp();
    await flushPromises();
    // Not expanded by default (no persisted explorerExpanded).
    expect(wrapper.findAll(".tree-session").length).toBe(0);
    await wrapper.get(".tree-folder").trigger("click");
    expect(wrapper.findAll(".tree-session").length).toBe(1);
    await wrapper.get(".tree-folder").trigger("click");
    expect(wrapper.findAll(".tree-session").length).toBe(0);
  });

  it("shows the empty state when there are no nodes at all", async () => {
    data.nodes = [];
    data.sessions = [];
    const wrapper = mountApp();
    await flushPromises();
    expect(wrapper.find(".explorer-empty").text()).toBe(
      "还没有内容。新建会话、文件夹，或点击上方的快速转写。",
    );
  });
});

// Context menu, inline rename, inline create and delete, wired to the node
// tree via App.vue's state (contextMenu/inlineCreate/renamingId/deleteTarget).
describe("explorer node actions", () => {
  function nodeActionsState(): AppData {
    return {
      nodes: [
        { id: "f1", parentId: null, kind: "folder", name: "Folder A" },
        { id: "f2", parentId: "f1", kind: "folder", name: "Nested" },
        { id: "s1", parentId: "f1", kind: "session", name: "Session One" },
        { id: "s2", parentId: "f2", kind: "session", name: "Session Two" },
        { id: "m1", parentId: "f1", kind: "material", name: "Notes.md" },
        { id: "s3", parentId: null, kind: "session", name: "Root Session" },
      ],
      sessions: [
        { id: "s1", messages: [], transcription: "", summary: "" },
        { id: "s2", messages: [], transcription: "", summary: "" },
        { id: "s3", messages: [], transcription: "", summary: "" },
      ],
      materials: [{ id: "m1", content: "hello", path: "/tmp/notes.md" }],
      settings: {
        providerId: "openai",
        model: "classroom-test",
        theme: "system",
        language: "zh",
        // Both folders start expanded so tests never need to click a folder
        // row (which would arm the real 500ms explorerExpanded persistence
        // timer and leak a stray save_settings invoke into a later test).
        explorerExpanded: ["f1", "f2"],
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

  // Mutates `data` the same way the real backend would, so refresh() (which
  // re-invokes load_state) observes the effect of the prior command.
  function mockNodeOps(data: AppData) {
    invoke.mockImplementation(async (command: string, args?: Record<string, any>) => {
      if (command === "load_state") return structuredClone(data);
      if (command === "stt_status")
        return { ready: true, modelName: "fixture", modelPath: "/tmp/model", sizeBytes: 1 };
      if (command === "create_session") {
        const node = {
          id: "new-session",
          parentId: (args?.parentId as string | null) ?? null,
          kind: "session" as const,
          name: args?.name as string,
        };
        data.nodes.push(node);
        data.sessions.push({ id: node.id, messages: [], transcription: "", summary: "" });
        return node;
      }
      if (command === "create_folder") {
        const node = {
          id: "new-folder",
          parentId: (args?.parentId as string | null) ?? null,
          kind: "folder" as const,
          name: args?.name as string,
        };
        data.nodes.push(node);
        return node;
      }
      if (command === "import_material") {
        const node = {
          id: "new-material",
          parentId: (args?.parentId as string | null) ?? null,
          kind: "material" as const,
          name: "picked.md",
        };
        data.nodes.push(node);
        data.materials.push({ id: node.id, content: "picked", path: args?.path as string });
        return node;
      }
      if (command === "rename_node") {
        const node = data.nodes.find((item) => item.id === args?.id);
        if (node) node.name = args?.name as string;
        return node;
      }
      if (command === "delete_node") {
        const removed = new Set(subtreeIds(data.nodes, args?.id as string));
        data.nodes = data.nodes.filter((item) => !removed.has(item.id));
        data.sessions = data.sessions.filter((item) => !removed.has(item.id));
        data.materials = data.materials.filter((item) => !removed.has(item.id));
        return undefined;
      }
      if (command === "save_settings") data.settings = args?.settings as AppData["settings"];
      return undefined;
    });
  }

  function menuItemTexts(wrapper: VueWrapper): string[] {
    return wrapper.findAll(".context-menu button").map((item) => item.text());
  }
  function clickMenuItem(wrapper: VueWrapper, text: string) {
    return buttonWithText(wrapper, text).trigger("click");
  }
  function treeRow(wrapper: VueWrapper, selector: string, name: string) {
    const match = wrapper.findAll(selector).find((item) => item.text() === name);
    if (!match) throw new Error(`Missing ${selector}: ${name}`);
    return match;
  }

  let data: AppData;
  beforeEach(() => {
    data = nodeActionsState();
    invoke.mockReset();
    mockNodeOps(data);
    vi.mocked(open).mockReset();
  });

  it("shows the right-click menu on the root blank area with only create actions", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get(".explorer-blank").trigger("contextmenu");
    expect(menuItemTexts(wrapper)).toEqual(["新建会话", "新建文件夹", "导入材料…"]);
  });

  it("shows New/Import plus Rename and Delete on a folder, no Open", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await treeRow(wrapper, ".tree-folder", "Folder A").trigger("contextmenu");
    expect(menuItemTexts(wrapper)).toEqual([
      "新建会话",
      "新建文件夹",
      "导入材料…",
      "重命名",
      "删除",
    ]);
  });

  it("shows only Open, Rename and Delete on a session or material", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await treeRow(wrapper, ".tree-session", "Session One").trigger("contextmenu");
    expect(menuItemTexts(wrapper)).toEqual(["打开", "重命名", "删除"]);
    await treeRow(wrapper, ".tree-material", "Notes.md").trigger("contextmenu");
    expect(menuItemTexts(wrapper)).toEqual(["打开", "重命名", "删除"]);
  });

  it("closes the context menu on Escape and on an outside click", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get(".explorer-blank").trigger("contextmenu");
    expect(wrapper.find(".context-menu").exists()).toBe(true);
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    await nextTick();
    expect(wrapper.find(".context-menu").exists()).toBe(false);

    await wrapper.get(".explorer-blank").trigger("contextmenu");
    document.body.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    await nextTick();
    expect(wrapper.find(".context-menu").exists()).toBe(false);
  });

  it("renames a session via F2: Enter commits the trimmed name", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await treeRow(wrapper, ".tree-session", "Session One").trigger("keydown", { key: "F2" });
    const input = wrapper.get(".inline-input");
    expect((input.element as HTMLInputElement).value).toBe("Session One");
    await input.setValue("  Renamed  ");
    await input.trigger("keydown", { key: "Enter" });
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("rename_node", { id: "s1", name: "Renamed" });
    expect(wrapper.find(".inline-input").exists()).toBe(false);
    expect(treeRow(wrapper, ".tree-session", "Renamed").exists()).toBe(true);
  });

  it("scrolls the renamed row into view after a commit", async () => {
    const wrapper = mountApp();
    await flushPromises();
    const scrollSpy = vi.spyOn(Element.prototype, "scrollIntoView");
    await treeRow(wrapper, ".tree-session", "Session One").trigger("keydown", { key: "F2" });
    await wrapper.get(".inline-input").setValue("Renamed");
    await wrapper.get(".inline-input").trigger("keydown", { key: "Enter" });
    await flushPromises();
    expect(scrollSpy).toHaveBeenCalledWith({ block: "nearest" });
    scrollSpy.mockRestore();
  });

  it("renames via the context menu's Rename item, Escape cancels without committing", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await treeRow(wrapper, ".tree-folder", "Folder A").trigger("contextmenu");
    await clickMenuItem(wrapper, "重命名");
    const input = wrapper.get(".inline-input");
    await input.trigger("keydown", { key: "Escape" });
    expect(wrapper.find(".inline-input").exists()).toBe(false);
    expect(invoke).not.toHaveBeenCalledWith("rename_node", expect.anything());
  });

  it("invokes nothing for a blank or unchanged rename", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await treeRow(wrapper, ".tree-folder", "Folder A").trigger("keydown", { key: "F2" });
    await wrapper.get(".inline-input").setValue("   ");
    await wrapper.get(".inline-input").trigger("keydown", { key: "Enter" });
    await flushPromises();
    expect(invoke).not.toHaveBeenCalledWith("rename_node", expect.anything());

    await treeRow(wrapper, ".tree-folder", "Folder A").trigger("keydown", { key: "F2" });
    await wrapper.get(".inline-input").trigger("keydown", { key: "Enter" });
    await flushPromises();
    expect(invoke).not.toHaveBeenCalledWith("rename_node", expect.anything());
  });

  it("pre-selects a material's base name, keeping the extension unselected", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await treeRow(wrapper, ".tree-material", "Notes.md").trigger("keydown", { key: "F2" });
    await flushPromises();
    const el = wrapper.get(".inline-input").element as HTMLInputElement;
    expect(el.selectionStart).toBe(0);
    expect(el.selectionEnd).toBe("Notes".length);
  });

  it("creates a session inside a folder via the context menu, resolved to that folder", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await treeRow(wrapper, ".tree-folder", "Folder A").trigger("contextmenu");
    await clickMenuItem(wrapper, "新建会话");
    await wrapper.get(".inline-input").setValue("New one");
    await wrapper.get(".inline-input").trigger("keydown", { key: "Enter" });
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("create_session", {
      parentId: "f1",
      name: "New one",
    });
  });

  it("creates a folder at root from the blank-area menu", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get(".explorer-blank").trigger("contextmenu");
    await clickMenuItem(wrapper, "新建文件夹");
    await wrapper.get(".inline-input").setValue("New folder");
    await wrapper.get(".inline-input").trigger("keydown", { key: "Enter" });
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("create_folder", { parentId: null, name: "New folder" });
  });

  it("Escape cancels inline create; a blank blur invokes nothing", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get(".explorer-blank").trigger("contextmenu");
    await clickMenuItem(wrapper, "新建会话");
    await wrapper.get(".inline-input").trigger("keydown", { key: "Escape" });
    expect(wrapper.find(".inline-input").exists()).toBe(false);

    await wrapper.get(".explorer-blank").trigger("contextmenu");
    await clickMenuItem(wrapper, "新建会话");
    await wrapper.get(".inline-input").trigger("blur");
    await flushPromises();
    expect(invoke).not.toHaveBeenCalledWith("create_session", expect.anything());
  });

  it("imports a material via the menu; a cancelled picker creates nothing", async () => {
    vi.mocked(open).mockResolvedValueOnce(null);
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get(".explorer-blank").trigger("contextmenu");
    await clickMenuItem(wrapper, "导入材料…");
    await flushPromises();
    expect(invoke).not.toHaveBeenCalledWith("import_material", expect.anything());

    vi.mocked(open).mockResolvedValueOnce("/tmp/picked.md");
    await wrapper.get(".explorer-blank").trigger("contextmenu");
    await clickMenuItem(wrapper, "导入材料…");
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("import_material", {
      parentId: null,
      path: "/tmp/picked.md",
    });
  });

  it("deletes a folder with a count-based confirmation, clearing the open node inside it", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await treeRow(wrapper, ".tree-session", "Session One").trigger("click");
    await flushPromises();
    expect(wrapper.find(".app-header h1").text()).toBe("Session One");

    await treeRow(wrapper, ".tree-folder", "Folder A").trigger("contextmenu");
    await clickMenuItem(wrapper, "删除");
    expect(wrapper.find(".dialog-stub").text()).toContain(
      "删除文件夹「Folder A」？其中的 1 个子文件夹、2 个会话（含转写、对话和摘要）和 1 份材料将被永久删除，无法撤销。",
    );
    await buttonWithText(wrapper, "删除").trigger("click");
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("delete_node", { id: "f1" });
    expect(wrapper.find(".workbench-empty").exists()).toBe(true);
    expect(wrapper.findAll(".tree-folder").length).toBe(0);
  });

  it("deletes a session and moves focus to the previous row when it was last", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await treeRow(wrapper, ".tree-session", "Root Session").trigger("contextmenu");
    await clickMenuItem(wrapper, "删除");
    expect(wrapper.find(".dialog-stub").text()).toContain(
      "删除会话「Root Session」？其转写、对话和摘要将被永久删除，无法撤销。",
    );
    await buttonWithText(wrapper, "删除").trigger("click");
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("delete_node", { id: "s3" });
    expect(wrapper.findAll(".tree-session").find((n) => n.text() === "Root Session")).toBeUndefined();
  });

  it("deletes a material with its own confirmation copy", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await treeRow(wrapper, ".tree-material", "Notes.md").trigger("contextmenu");
    await clickMenuItem(wrapper, "删除");
    expect(wrapper.find(".dialog-stub").text()).toContain(
      "删除材料「Notes.md」？将被永久删除，无法撤销。",
    );
    await buttonWithText(wrapper, "删除").trigger("click");
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("delete_node", { id: "m1" });
  });

  it("cancel on the delete dialog invokes nothing", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await treeRow(wrapper, ".tree-session", "Root Session").trigger("contextmenu");
    await clickMenuItem(wrapper, "删除");
    await buttonWithText(wrapper, "取消").trigger("click");
    expect(wrapper.find(".dialog-stub").exists()).toBe(false);
    expect(invoke).not.toHaveBeenCalledWith("delete_node", expect.anything());
  });

  describe("while an operation is busy", () => {
    async function makeBusy(wrapper: VueWrapper) {
      await treeRow(wrapper, ".tree-session", "Root Session").trigger("click");
      await flushPromises();
      await buttonWithText(wrapper, "开始录音").trigger("click");
      await flushPromises();
    }

    it("refuses to open the context menu and closes one already open", async () => {
      const wrapper = mountApp();
      await flushPromises();
      await treeRow(wrapper, ".tree-session", "Root Session").trigger("contextmenu");
      expect(wrapper.find(".context-menu").exists()).toBe(true);
      await makeBusy(wrapper);
      expect(wrapper.find(".context-menu").exists()).toBe(false);
      await treeRow(wrapper, ".tree-folder", "Folder A").trigger("contextmenu");
      expect(wrapper.find(".context-menu").exists()).toBe(false);
    });

    it("closes an open rename box without committing once recording starts", async () => {
      const wrapper = mountApp();
      await flushPromises();
      await treeRow(wrapper, ".tree-folder", "Folder A").trigger("keydown", { key: "F2" });
      expect(wrapper.find(".inline-input").exists()).toBe(true);
      await makeBusy(wrapper);
      expect(wrapper.find(".inline-input").exists()).toBe(false);
      expect(invoke).not.toHaveBeenCalledWith("rename_node", expect.anything());
    });

    it("ignores F2 while busy", async () => {
      const wrapper = mountApp();
      await flushPromises();
      await makeBusy(wrapper);
      await treeRow(wrapper, ".tree-folder", "Folder A").trigger("keydown", { key: "F2" });
      expect(wrapper.find(".inline-input").exists()).toBe(false);
    });

    it("disables the delete confirm button without closing the dialog", async () => {
      const wrapper = mountApp();
      await flushPromises();
      await treeRow(wrapper, ".tree-folder", "Folder A").trigger("contextmenu");
      await clickMenuItem(wrapper, "删除");
      await makeBusy(wrapper);
      expect(wrapper.find(".dialog-stub").exists()).toBe(true);
      expect(buttonWithText(wrapper, "删除").attributes("disabled")).toBeDefined();
    });
  });
});

// Native HTML5 drag and drop, wired through App.vue's dragNodeId/dropParent
// state to ExplorerView's drag handlers and the move_node command.
describe("explorer drag and drop", () => {
  function dragDropState(): AppData {
    return {
      nodes: [
        { id: "f1", parentId: null, kind: "folder", name: "Folder A" },
        { id: "f2", parentId: "f1", kind: "folder", name: "Nested" },
        { id: "s1", parentId: "f1", kind: "session", name: "Session One" },
        { id: "s3", parentId: null, kind: "session", name: "Root Session" },
      ],
      sessions: [
        { id: "s1", messages: [], transcription: "", summary: "" },
        { id: "s3", messages: [], transcription: "", summary: "" },
      ],
      materials: [],
      settings: {
        providerId: "openai",
        model: "classroom-test",
        theme: "system",
        language: "zh",
        // Both folders start expanded so drop targets are on screen without
        // needing a real click (which would arm the 500ms persistence timer).
        explorerExpanded: ["f1", "f2"],
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

  function mockDragDrop(data: AppData) {
    invoke.mockImplementation(async (command: string, args?: Record<string, any>) => {
      if (command === "load_state") return structuredClone(data);
      if (command === "stt_status")
        return { ready: true, modelName: "fixture", modelPath: "/tmp/model", sizeBytes: 1 };
      if (command === "move_node") {
        const node = data.nodes.find((item) => item.id === args?.id);
        if (node) node.parentId = (args?.parentId as string | null) ?? null;
        return node;
      }
      if (command === "save_settings") data.settings = args?.settings as AppData["settings"];
      return undefined;
    });
  }

  function fakeDataTransfer(types: string[] = ["application/x-async-node"]) {
    return {
      types,
      effectAllowed: "",
      dropEffect: "",
      setData: vi.fn(),
      getData: vi.fn(() => ""),
    };
  }
  function treeRow(wrapper: VueWrapper, selector: string, name: string) {
    const match = wrapper.findAll(selector).find((item) => item.text() === name);
    if (!match) throw new Error(`Missing ${selector}: ${name}`);
    return match;
  }
  // The draggable/dragstart/dragover/drop listeners live on the row div, one
  // level up from the .tree-node button treeRow() finds.
  function rowDiv(wrapper: VueWrapper, name: string) {
    const match = wrapper.findAll(".tree-row").find((item) => item.text().includes(name));
    if (!match) throw new Error(`Missing row: ${name}`);
    return match;
  }

  let data: AppData;
  beforeEach(() => {
    data = dragDropState();
    invoke.mockReset();
    mockDragDrop(data);
  });

  it("moves a node into another folder on drop", async () => {
    const wrapper = mountApp();
    await flushPromises();
    const dataTransfer = fakeDataTransfer();
    await rowDiv(wrapper, "Session One").trigger("dragstart", { dataTransfer });
    await rowDiv(wrapper, "Nested").trigger("dragover", { dataTransfer });
    await rowDiv(wrapper, "Nested").trigger("drop", { dataTransfer });
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("move_node", { id: "s1", parentId: "f2" });
  });

  it("scrolls the moved row into view after a successful drop", async () => {
    const wrapper = mountApp();
    await flushPromises();
    const scrollSpy = vi.spyOn(Element.prototype, "scrollIntoView");
    const dataTransfer = fakeDataTransfer();
    await rowDiv(wrapper, "Session One").trigger("dragstart", { dataTransfer });
    await rowDiv(wrapper, "Nested").trigger("dragover", { dataTransfer });
    await rowDiv(wrapper, "Nested").trigger("drop", { dataTransfer });
    await flushPromises();
    expect(scrollSpy).toHaveBeenCalledWith({ block: "nearest" });
    scrollSpy.mockRestore();
  });

  it("resolves a drop onto a session row to that session's own parent", async () => {
    const wrapper = mountApp();
    await flushPromises();
    const dataTransfer = fakeDataTransfer();
    // Root Session's parent is root (null), so dropping "on" it moves into root.
    await rowDiv(wrapper, "Session One").trigger("dragstart", { dataTransfer });
    await rowDiv(wrapper, "Root Session").trigger("dragover", { dataTransfer });
    await rowDiv(wrapper, "Root Session").trigger("drop", { dataTransfer });
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("move_node", { id: "s1", parentId: null });
  });

  it("refuses dropping a folder onto itself or its own descendant", async () => {
    const wrapper = mountApp();
    await flushPromises();
    const dataTransfer = fakeDataTransfer();
    await rowDiv(wrapper, "Folder A").trigger("dragstart", { dataTransfer });
    await rowDiv(wrapper, "Folder A").trigger("dragover", { dataTransfer });
    await rowDiv(wrapper, "Folder A").trigger("drop", { dataTransfer });
    await rowDiv(wrapper, "Nested").trigger("dragover", { dataTransfer });
    await rowDiv(wrapper, "Nested").trigger("drop", { dataTransfer });
    await flushPromises();
    expect(invoke).not.toHaveBeenCalledWith("move_node", expect.anything());
  });

  it("moves a node to root by dropping on the blank area", async () => {
    const wrapper = mountApp();
    await flushPromises();
    const dataTransfer = fakeDataTransfer();
    await rowDiv(wrapper, "Session One").trigger("dragstart", { dataTransfer });
    await wrapper.get(".explorer-blank").trigger("dragover", { dataTransfer });
    await wrapper.get(".explorer-blank").trigger("drop", { dataTransfer });
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("move_node", { id: "s1", parentId: null });
  });

  it("tints every row while dragging over the root blank area, like a folder target tints its descendants", async () => {
    const wrapper = mountApp();
    await flushPromises();
    const dataTransfer = fakeDataTransfer();
    await rowDiv(wrapper, "Session One").trigger("dragstart", { dataTransfer });
    await wrapper.get(".explorer-blank").trigger("dragover", { dataTransfer });
    expect(rowDiv(wrapper, "Folder A").classes()).toContain("drop-descendant");
    expect(rowDiv(wrapper, "Nested").classes()).toContain("drop-descendant");
    expect(rowDiv(wrapper, "Root Session").classes()).toContain("drop-descendant");
  });

  it("ignores a dragover/drop carrying a foreign MIME type", async () => {
    const wrapper = mountApp();
    await flushPromises();
    const validTransfer = fakeDataTransfer();
    await rowDiv(wrapper, "Session One").trigger("dragstart", { dataTransfer: validTransfer });
    const foreignTransfer = fakeDataTransfer(["text/uri-list"]);
    await rowDiv(wrapper, "Nested").trigger("dragover", { dataTransfer: foreignTransfer });
    await rowDiv(wrapper, "Nested").trigger("drop", { dataTransfer: foreignTransfer });
    await flushPromises();
    expect(invoke).not.toHaveBeenCalledWith("move_node", expect.anything());
  });

  it("refuses to drag or drop while an operation is busy", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await treeRow(wrapper, ".tree-session", "Root Session").trigger("click");
    await flushPromises();
    await buttonWithText(wrapper, "开始录音").trigger("click");
    await flushPromises();
    expect(rowDiv(wrapper, "Session One").attributes("draggable")).toBe("false");

    const dataTransfer = fakeDataTransfer();
    await rowDiv(wrapper, "Session One").trigger("dragstart", { dataTransfer });
    await rowDiv(wrapper, "Nested").trigger("dragover", { dataTransfer });
    await rowDiv(wrapper, "Nested").trigger("drop", { dataTransfer });
    await flushPromises();
    expect(invoke).not.toHaveBeenCalledWith("move_node", expect.anything());
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
    await openSessionNode(wrapper, "Lesson one");
    await buttonWithText(wrapper, "复制").trigger("click");
    await flushPromises();
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith("Hello");
  });

  it("persists the shortened message list via save_messages when deleting a message", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Lesson one");
    await wrapper
      .findAll(".message-actions")[0]!
      .findAll("button")
      .find((item) => item.text() === "删除")!
      .trigger("click");
    await flushPromises();
    const call = invoke.mock.calls.find(([command]) => command === "save_messages");
    expect(call).toBeTruthy();
    const args = call![1] as any;
    expect(args.sessionId).toBe("s1");
    expect(args.messages).toHaveLength(1);
    expect(args.messages[0].id).toBe("m2");
  });

  it("drops the old assistant turn and re-invokes chat when regenerating", async () => {
    const savedMessageSnapshots: unknown[] = [];
    invoke.mockImplementation((command: string, args?: Record<string, any>) => {
      if (command === "load_state") return Promise.resolve(structuredClone(data));
      if (command === "stt_status")
        return Promise.resolve({
          ready: true,
          modelName: "fixture",
          modelPath: "/tmp",
          sizeBytes: 1,
        });
      if (command === "save_messages") {
        savedMessageSnapshots.push(JSON.parse(JSON.stringify(args?.messages)));
        const session = data.sessions.find((item) => item.id === args?.sessionId);
        if (session) session.messages = args?.messages;
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
    await openSessionNode(wrapper, "Lesson one");
    await wrapper
      .findAll(".message-actions")[1]!
      .findAll("button")
      .find((item) => item.text() === "重新生成")!
      .trigger("click");
    await flushPromises();
    expect(savedMessageSnapshots).toHaveLength(1);
    expect(savedMessageSnapshots[0]).toHaveLength(0);
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
    await openSessionNode(wrapper, "Lesson one");
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
    await openSessionNode(wrapper, "Lesson one");
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
    await openSessionNode(wrapper, "Lesson one");
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
    await openSessionNode(wrapper, "Lesson one");
    await buttonWithText(wrapper, "摘要").trigger("click");
    expect(wrapper.find(".summary-meta").exists()).toBe(true);
    expect(wrapper.find(".summary-meta").text()).toContain("更新时间：");

    data.sessions[0]!.summaryUpdatedAt = null;
    const noDateWrapper = mountApp();
    await flushPromises();
    await openSessionNode(noDateWrapper, "Lesson one");
    await buttonWithText(noDateWrapper, "摘要").trigger("click");
    expect(noDateWrapper.find(".summary-meta").exists()).toBe(false);
  });

  it("remembers the chat/summary tab selection per session across a session switch", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Lesson one");
    await buttonWithText(wrapper, "摘要").trigger("click");
    expect(wrapper.find(".summary-page").exists()).toBe(true);
    await wrapper.findAll(".tree-session")[1]!.trigger("click");
    expect(wrapper.find(".summary-page").exists()).toBe(false);
    await wrapper.findAll(".tree-session")[0]!.trigger("click");
    expect(wrapper.find(".summary-page").exists()).toBe(true);
  });

  it("renders the workbench empty state with no chrome before any node is opened", async () => {
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
    await openSessionNode(wrapper, "Lesson one");
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

// A stale message-edit box committed while a chat stream is live would
// clobber data the backend is still writing.
describe("operationBusy guards on message affordances", () => {
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
      if (command === "chat") return new Promise(() => undefined); // never resolves: stream stays in flight
      return Promise.resolve();
    });
  }

  async function startStreaming(wrapper: VueWrapper) {
    await wrapper.get("form.composer textarea").setValue("Another question");
    await wrapper.get("form.composer").trigger("submit");
    await flushPromises();
  }

  it("closes an open message-edit box and never saves it once a chat stream starts", async () => {
    mockInvokeWithPendingChat();
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Lesson one");
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
      invoke.mock.calls.some(([command]) => command === "save_messages"),
    ).toBe(false);
    expect(wrapper.text()).not.toContain("Edited content");
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
      if (command === "save_messages")
        return new Promise<void>((resolve) => {
          releaseSave = resolve;
        });
      return Promise.resolve();
    });
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Lesson one");
    await wrapper
      .findAll(".message-actions")[1]!
      .findAll("button")
      .find((item) => item.text() === "重新生成")!
      .trigger("click");
    await nextTick();
    // regenerateMessage is now paused awaiting save_messages; streaming must
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
    data.nodes = [
      { id: "mat1", parentId: null, kind: "material", name: "Notes.md" },
      { id: "mat2", parentId: null, kind: "material", name: "Other.md" },
    ];
    data.materials = [
      { id: "mat1", content: "original content", path: "/tmp/notes.md" },
      { id: "mat2", content: "other content", path: "/tmp/other.md" },
    ];
    invoke.mockReset();
    mockInvoke(data);
  });

  it("keeps an unsaved draft across an unrelated refresh, but swaps it when a different material is selected", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get(".tree-material").trigger("click");
    expect(
      (wrapper.get(".material-editor textarea").element as HTMLTextAreaElement)
        .value,
    ).toBe("original content");
    await wrapper.get(".material-editor textarea").setValue("unsaved edits");

    // Save settings: an action wholly unrelated to the open material, but one
    // that still triggers a refresh() and a fresh load_state payload.
    await buttonWithText(wrapper, "设置").trigger("click");
    await buttonWithText(wrapper, "保存设置").trigger("click");
    await flushPromises();
    expect(
      invoke.mock.calls.some(([command]) => command === "save_settings"),
    ).toBe(true);
    expect(
      (wrapper.get(".material-editor textarea").element as HTMLTextAreaElement)
        .value,
    ).toBe("unsaved edits");

    // Selecting a different material still swaps the draft as expected.
    const materialRows = wrapper.findAll(".tree-material");
    await materialRows[1]!.trigger("click");
    await nextTick();
    expect(
      (wrapper.get(".material-editor textarea").element as HTMLTextAreaElement)
        .value,
    ).toBe("other content");
  });

  it("saves a material with the id/content payload the backend command expects", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get(".tree-material").trigger("click");
    await wrapper.get(".material-editor textarea").setValue("edited content");
    await buttonWithText(wrapper, "保存").trigger("click");
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith("save_material", {
      id: "mat1",
      content: "edited content",
    });
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
    const savedMessageSnapshots: unknown[] = [];
    invoke.mockImplementation((command: string, args?: Record<string, any>) => {
      if (command === "load_state") return Promise.resolve(structuredClone(data));
      if (command === "stt_status")
        return Promise.resolve({
          ready: true,
          modelName: "fixture",
          modelPath: "/tmp",
          sizeBytes: 1,
        });
      if (command === "save_messages") {
        savedMessageSnapshots.push(JSON.parse(JSON.stringify(args?.messages)));
        const session = data.sessions.find((item) => item.id === args?.sessionId);
        if (session) session.messages = args?.messages;
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
    await openSessionNode(wrapper, "Lesson one");
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
    expect(savedMessageSnapshots).toHaveLength(1);
    expect(savedMessageSnapshots[0]).toHaveLength(0);
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
    await openSessionNode(wrapper, "Lesson one");
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
    await openSessionNode(wrapper, "Lesson one");
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
    await openSessionNode(wrapper, "Lesson one");
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
    await openSessionNode(wrapper, "Lesson one");
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
    await openSessionNode(wrapper, "Lesson one");
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
    await openSessionNode(wrapper, "Lesson one");
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
    await openSessionNode(wrapper, "Lesson one");
    const modeSelect = wrapper.get('[aria-label="输入方式"]');
    const systemOption = modeSelect
      .findAll("option")
      .find((option) => option.element.value === "system")!;
    expect(systemOption.attributes("disabled")).toBeDefined();
    expect(systemOption.text()).toContain("暂不可用");
  });

  it("enables the system-audio option once the backend reports it is available, and records with that source", async () => {
    invoke.mockImplementation(
      async (command: string, args?: Record<string, any>) => {
        if (command === "get_system_audio_capability")
          return { available: true, reason: null };
        if (command === "load_state") return structuredClone(data);
        if (command === "stt_status")
          return {
            ready: true,
            modelName: "fixture",
            modelPath: "/tmp/model",
            sizeBytes: 1,
          };
        return undefined;
      },
    );
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Lesson one");
    const modeSelect = wrapper.get('[aria-label="输入方式"]');
    const systemOption = modeSelect
      .findAll("option")
      .find((option) => option.element.value === "system")!;
    expect(systemOption.attributes("disabled")).toBeUndefined();
    expect(systemOption.text()).not.toContain("暂不可用");

    await modeSelect.setValue("system");
    await nextTick();
    await buttonWithText(wrapper, "开始录音").trigger("click");
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith(
      "start_recording",
      expect.objectContaining({ sessionId: "s1", source: "systemAudio" }),
    );
  });

  it("passes the picked transcription language into transcribe_audio", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Lesson one");
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
    await openSessionNode(wrapper, "Lesson one");
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
    await openSessionNode(wrapper, "Lesson one");
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
    await openSessionNode(wrapper, "Lesson one");
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

    // Removing the <audio> element from the DOM does not reliably stop
    // playback, so the source change must pause it explicitly first -
    // otherwise leftover playback can bleed into a fresh recording capture.
    const pauseSpy = vi.spyOn(audio, "pause");
    await wrapper.findAll(".tree-session")[1]!.trigger("click");
    await flushPromises();
    expect(pauseSpy).toHaveBeenCalled();
    expect(revokeSpy).toHaveBeenCalled();
    expect(wrapper.find(".playback-bar").exists()).toBe(false);
  });
});

// One click creates a root session named with the local timestamp and
// starts recording in the currently selected capture mode. The mocked
// start_quick_transcription/create_session/delete_node handlers below mutate
// `data` the same way the real backend's node tree would, so a fresh
// load_state call after a failure is a genuine check for an orphaned node -
// not just an assertion on which invoke calls were made.
describe("quick transcription", () => {
  function quickState(): AppData {
    return {
      nodes: [{ id: "existing", parentId: null, kind: "session", name: "Existing" }],
      sessions: [{ id: "existing", messages: [], transcription: "", summary: "" }],
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

  function mockQuick(
    state: AppData,
    opts: {
      sttReady?: boolean;
      systemAvailable?: boolean;
      startQuickRejects?: string;
    } = {},
  ) {
    invoke.mockImplementation(async (command: string, args?: Record<string, any>) => {
      if (command === "load_state") return structuredClone(state);
      if (command === "stt_status")
        return {
          ready: opts.sttReady ?? true,
          modelName: "fixture",
          modelPath: "/tmp/model",
          sizeBytes: 1,
        };
      if (command === "get_system_audio_capability")
        return {
          available: opts.systemAvailable ?? false,
          reason: opts.systemAvailable ? null : "unsupported-os",
        };
      if (command === "start_quick_transcription") {
        // Mirrors the backend: the node (and its session payload) is
        // inserted before start() is awaited, then deleted again if start
        // fails - never left behind.
        state.nodes.push({ id: args!.id, parentId: null, kind: "session", name: args!.name });
        state.sessions.push({ id: args!.id, messages: [], transcription: "", summary: "" });
        if (opts.startQuickRejects) {
          state.nodes = state.nodes.filter((n) => n.id !== args!.id);
          state.sessions = state.sessions.filter((s) => s.id !== args!.id);
          throw new Error(opts.startQuickRejects);
        }
        return { id: args!.id, parentId: null, kind: "session", name: args!.name };
      }
      if (command === "create_session") {
        const node = {
          id: "upload-session",
          parentId: args?.parentId ?? null,
          kind: "session" as const,
          name: args?.name,
        };
        state.nodes.push(node);
        state.sessions.push({ id: node.id, messages: [], transcription: "", summary: "" });
        return node;
      }
      if (command === "delete_node") {
        state.nodes = state.nodes.filter((n) => n.id !== args?.id);
        state.sessions = state.sessions.filter((s) => s.id !== args?.id);
        return undefined;
      }
      if (command === "transcribe_audio") return undefined;
      if (command === "save_settings") {
        state.settings = args?.settings as AppData["settings"];
      }
      return undefined;
    });
  }

  let data: AppData;
  beforeEach(() => {
    data = quickState();
    invoke.mockReset();
    vi.mocked(open).mockReset();
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2024, 2, 5, 10, 20, 30));
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("creates a root session named with the current timestamp and starts recording in the current capture mode", async () => {
    mockQuick(data);
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get(".quick-transcription-btn").trigger("click");
    await flushPromises();
    const call = invoke.mock.calls.find(
      ([command]) => command === "start_quick_transcription",
    )!;
    expect(call[1]).toMatchObject({
      name: "2024-03-05 10:20:30",
      source: "microphone",
      language: null,
    });
    expect(data.nodes).toHaveLength(2);
    expect(wrapper.get("h1").text()).toBe("2024-03-05 10:20:30");
    expect(buttonWithText(wrapper, "停止录音").exists()).toBe(true);
  });

  it("honors the currently selected capture mode, starting system-audio capture when that mode is selected", async () => {
    mockQuick(data, { systemAvailable: true });
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Existing");
    await wrapper.get('[aria-label="输入方式"]').setValue("system");
    await wrapper.get(".quick-transcription-btn").trigger("click");
    await flushPromises();
    const call = invoke.mock.calls.find(
      ([command]) => command === "start_quick_transcription",
    )!;
    expect(call[1]).toMatchObject({ source: "systemAudio" });
  });

  it("refuses cleanly when no local STT model is ready, creating nothing", async () => {
    mockQuick(data, { sttReady: false });
    const wrapper = mountApp();
    await flushPromises();
    const before = data.nodes.length;
    await wrapper.get(".quick-transcription-btn").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("需要先下载本地语音模型才能快速转写");
    expect(
      invoke.mock.calls.some(([command]) => command === "start_quick_transcription"),
    ).toBe(false);
    const fresh = (await invoke("load_state")) as AppData;
    expect(fresh.nodes).toHaveLength(before);
  });

  it("is refused while a recording is already in progress elsewhere, without creating a session", async () => {
    mockQuick(data);
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Existing");
    await buttonWithText(wrapper, "开始录音").trigger("click");
    await flushPromises();
    const before = data.nodes.length;
    invoke.mockClear();
    const quickButton = wrapper.get(".quick-transcription-btn");
    expect(quickButton.attributes("disabled")).toBeDefined();
    await quickButton.trigger("click");
    await flushPromises();
    expect(
      invoke.mock.calls.some(([command]) => command === "start_quick_transcription"),
    ).toBe(false);
    const fresh = (await invoke("load_state")) as AppData;
    expect(fresh.nodes).toHaveLength(before);
  });

  it("deletes the session when starting capture fails, leaving no orphan session behind", async () => {
    mockQuick(data, { startQuickRejects: "麦克风权限被拒绝。" });
    const wrapper = mountApp();
    await flushPromises();
    const before = data.nodes.length;
    await wrapper.get(".quick-transcription-btn").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("麦克风权限被拒绝");
    const fresh = (await invoke("load_state")) as AppData;
    expect(fresh.nodes).toHaveLength(before);
    expect(fresh.nodes.some((node) => node.name === "2024-03-05 10:20:30")).toBe(false);
    // The button unlocks again rather than getting stuck disabled.
    expect(wrapper.get(".quick-transcription-btn").attributes("disabled")).toBeUndefined();
  });

  it("cancels and shows the empty-session dialog when an error event arrives before start_quick_transcription's own reply", async () => {
    let resolveStart: (node: unknown) => void = () => undefined;
    let captured: Record<string, any> | undefined;
    invoke.mockImplementation(async (command: string, args?: Record<string, any>) => {
      if (command === "load_state") return structuredClone(data);
      if (command === "stt_status")
        return { ready: true, modelName: "f", modelPath: "/tmp", sizeBytes: 1 };
      if (command === "start_quick_transcription") {
        captured = args;
        data.nodes.push({ id: args!.id, parentId: null, kind: "session", name: args!.name });
        data.sessions.push({ id: args!.id, messages: [], transcription: "", summary: "" });
        return new Promise((resolve) => {
          resolveStart = resolve;
        });
      }
      if (command === "delete_node") {
        data.nodes = data.nodes.filter((n) => n.id !== args?.id);
        data.sessions = data.sessions.filter((s) => s.id !== args?.id);
        return undefined;
      }
      return undefined;
    });
    const wrapper = mountApp();
    await flushPromises();
    await wrapper.get(".quick-transcription-btn").trigger("click");
    await flushPromises();
    captured!.onEvent.onmessage({
      type: "error",
      sessionId: captured!.id,
      text: "设备被占用",
    });
    await flushPromises();
    expect(
      invoke.mock.calls.some(
        ([command, args]) =>
          command === "cancel_recording" && args?.sessionId === captured!.id,
      ),
    ).toBe(true);

    resolveStart({ id: captured!.id, parentId: null, kind: "session", name: captured!.name });
    await flushPromises();
    expect(wrapper.text()).toContain("没有录到内容");
    // The button unlocks again once the delayed reply is handled, rather
    // than getting stuck disabled by the now-superseded generation.
    expect(wrapper.get(".quick-transcription-btn").attributes("disabled")).toBeUndefined();

    await buttonWithText(wrapper, "删除").trigger("click");
    await flushPromises();
    expect(
      invoke.mock.calls.some(
        ([command, args]) => command === "delete_node" && args?.id === captured!.id,
      ),
    ).toBe(true);
    expect(data.nodes.some((node) => node.id === captured!.id)).toBe(false);
  });

  it("upload mode: a cancelled picker creates nothing; a chosen file creates the session and transcribes into it", async () => {
    mockQuick(data);
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Existing");
    await wrapper.get('[aria-label="输入方式"]').setValue("upload");

    vi.mocked(open).mockResolvedValueOnce(null);
    await wrapper.get(".quick-transcription-btn").trigger("click");
    await flushPromises();
    expect(
      invoke.mock.calls.some(([command]) => command === "create_session"),
    ).toBe(false);

    vi.mocked(open).mockResolvedValueOnce("/tmp/quick.wav");
    await wrapper.get(".quick-transcription-btn").trigger("click");
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith(
      "create_session",
      expect.objectContaining({ parentId: null, name: "2024-03-05 10:20:30" }),
    );
    expect(invoke).toHaveBeenCalledWith(
      "transcribe_audio",
      expect.objectContaining({ sessionId: "upload-session", path: "/tmp/quick.wav" }),
    );
    expect(wrapper.get("h1").text()).toBe("2024-03-05 10:20:30");
  });
});

describe("activity bar", () => {
  let data: AppData;
  let attached: VueWrapper | undefined;
  beforeEach(() => {
    data = baseState();
    invoke.mockReset();
    mockInvoke(data);
  });
  afterEach(() => {
    attached?.unmount();
    attached = undefined;
  });
  // Focus assertions need the wrapper attached to document.body - jsdom only
  // tracks document.activeElement for elements actually in the document.
  function mountAttached() {
    attached = mount(App, { global: { stubs }, attachTo: document.body });
    return attached;
  }

  function activityButton(wrapper: VueWrapper, label: string) {
    const button = wrapper.find(`.activity-bar [aria-label="${label}"]`);
    if (!button.exists()) throw new Error(`Missing activity bar button: ${label}`);
    return button;
  }

  it("starts on Explorer and switches to Search by clicking its icon, opening the panel if closed", async () => {
    const wrapper = mountApp();
    await flushPromises();
    expect(wrapper.find('[role="tree"]').exists()).toBe(true);
    expect(wrapper.find(".search-view").exists()).toBe(false);
    expect(activityButton(wrapper, "资源管理器").attributes("aria-pressed")).toBe("true");
    expect(activityButton(wrapper, "搜索").attributes("aria-pressed")).toBe("false");

    await activityButton(wrapper, "搜索").trigger("click");
    expect(wrapper.find(".search-view").exists()).toBe(true);
    expect(wrapper.find('[role="tree"]').exists()).toBe(false);
    expect(activityButton(wrapper, "搜索").attributes("aria-pressed")).toBe("true");
    expect(activityButton(wrapper, "资源管理器").attributes("aria-pressed")).toBe("false");
  });

  it("clicking the active view's icon collapses the panel; clicking it again reopens the same view", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await activityButton(wrapper, "资源管理器").trigger("click");
    expect(wrapper.find('[role="tree"]').exists()).toBe(false);
    expect(wrapper.find(".search-view").exists()).toBe(false);

    await activityButton(wrapper, "资源管理器").trigger("click");
    expect(wrapper.find('[role="tree"]').exists()).toBe(true);
  });

  it("Ctrl+Shift+F opens Search and focuses the query input; Ctrl+Shift+E returns to Explorer and focuses a row", async () => {
    const wrapper = mountAttached();
    await flushPromises();
    window.dispatchEvent(
      new KeyboardEvent("keydown", { key: "F", ctrlKey: true, shiftKey: true, bubbles: true }),
    );
    await flushPromises();
    expect(wrapper.find(".search-view").exists()).toBe(true);
    expect(document.activeElement?.tagName).toBe("INPUT");

    window.dispatchEvent(
      new KeyboardEvent("keydown", { key: "E", ctrlKey: true, shiftKey: true, bubbles: true }),
    );
    await flushPromises();
    expect(wrapper.find('[role="tree"]').exists()).toBe(true);
    expect(document.activeElement?.classList.contains("tree-node")).toBe(true);
  });

  it("ignores the view shortcuts while an IME composition is in progress", async () => {
    const wrapper = mountApp();
    await flushPromises();
    window.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: "F",
        ctrlKey: true,
        shiftKey: true,
        isComposing: true,
        bubbles: true,
      }),
    );
    await flushPromises();
    expect(wrapper.find(".search-view").exists()).toBe(false);
  });

  it("keeps Ctrl+B toggling the sidebar for whichever view is active", async () => {
    const wrapper = mountApp();
    await flushPromises();
    await activityButton(wrapper, "搜索").trigger("click");
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "b", ctrlKey: true, bubbles: true }));
    await flushPromises();
    expect(wrapper.find(".search-view").exists()).toBe(false);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "b", ctrlKey: true, bubbles: true }));
    await flushPromises();
    expect(wrapper.find(".search-view").exists()).toBe(true);
  });
});

describe("search view", () => {
  function searchState(): AppData {
    return {
      nodes: [
        { id: "f1", parentId: null, kind: "folder", name: "Folder A" },
        { id: "s1", parentId: "f1", kind: "session", name: "Deep Session" },
        { id: "s2", parentId: null, kind: "session", name: "Root Session" },
        { id: "m1", parentId: "f1", kind: "material", name: "Notes.md" },
      ],
      sessions: [
        { id: "s1", messages: [], transcription: "", summary: "" },
        { id: "s2", messages: [], transcription: "", summary: "" },
      ],
      materials: [{ id: "m1", content: "hello", path: "/tmp/notes.md" }],
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

  function mockSearch(
    state: AppData,
    impl: (query: string) => { hits: unknown[]; truncated: boolean },
  ) {
    invoke.mockImplementation(async (command: string, args?: Record<string, any>) => {
      if (command === "load_state") return structuredClone(state);
      if (command === "stt_status")
        return { ready: true, modelName: "fixture", modelPath: "/tmp/model", sizeBytes: 1 };
      if (command === "search_library") return impl(args?.query as string);
      return undefined;
    });
  }

  async function openSearchView(wrapper: VueWrapper) {
    await wrapper.get('.activity-bar [aria-label="搜索"]').trigger("click");
  }
  function queryInput(wrapper: VueWrapper) {
    return wrapper.get(".search-field-wrap input");
  }

  let data: AppData;
  let attached: VueWrapper | undefined;
  beforeEach(() => {
    data = searchState();
    invoke.mockReset();
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
    attached?.unmount();
    attached = undefined;
  });
  // Focus assertions need the wrapper attached to document.body - jsdom only
  // tracks document.activeElement for elements actually in the document.
  function mountAttached() {
    attached = mount(App, { global: { stubs }, attachTo: document.body });
    return attached;
  }

  it("only issues one debounced search_library call while typing rapidly", async () => {
    let calls = 0;
    invoke.mockImplementation(async (command: string) => {
      if (command === "load_state") return structuredClone(data);
      if (command === "stt_status")
        return { ready: true, modelName: "fixture", modelPath: "/tmp/model", sizeBytes: 1 };
      if (command === "search_library") {
        calls += 1;
        return { hits: [], truncated: false };
      }
      return undefined;
    });
    const wrapper = mountApp();
    await flushPromises();
    await openSearchView(wrapper);
    const input = queryInput(wrapper);
    await input.setValue("a");
    await vi.advanceTimersByTimeAsync(120);
    await input.setValue("ab");
    await vi.advanceTimersByTimeAsync(120);
    await input.setValue("abc");
    await vi.advanceTimersByTimeAsync(200);
    await flushPromises();
    expect(calls).toBe(1);
    expect(invoke).toHaveBeenCalledWith("search_library", { query: "abc" });
  });

  it("ignores a stale response that resolves after a newer query was already issued", async () => {
    let resolveFirst!: (value: { hits: unknown[]; truncated: boolean }) => void;
    let resolveSecond!: (value: { hits: unknown[]; truncated: boolean }) => void;
    invoke.mockImplementation(async (command: string, args?: Record<string, any>) => {
      if (command === "load_state") return structuredClone(data);
      if (command === "stt_status")
        return { ready: true, modelName: "fixture", modelPath: "/tmp/model", sizeBytes: 1 };
      if (command === "search_library") {
        if (args?.query === "first") return new Promise((resolve) => (resolveFirst = resolve));
        return new Promise((resolve) => (resolveSecond = resolve));
      }
      return undefined;
    });
    const wrapper = mountApp();
    await flushPromises();
    await openSearchView(wrapper);
    const input = queryInput(wrapper);
    await input.setValue("first");
    await vi.advanceTimersByTimeAsync(200);
    await flushPromises();
    await input.setValue("second");
    await vi.advanceTimersByTimeAsync(200);
    await flushPromises();

    resolveSecond({
      hits: [
        {
          nodeId: "s2",
          kind: "session",
          field: "name",
          piece: { before: "", matched: "Root Session", after: "" },
        },
      ],
      truncated: false,
    });
    await flushPromises();
    resolveFirst({
      hits: [
        {
          nodeId: "s1",
          kind: "session",
          field: "name",
          piece: { before: "", matched: "Deep Session", after: "" },
        },
      ],
      truncated: false,
    });
    await flushPromises();
    expect(wrapper.text()).toContain("Root Session");
    expect(wrapper.text()).not.toContain("Deep Session");
  });

  it("groups hits by node with muted parent paths, and always renders content as text, never HTML", async () => {
    mockSearch(data, () => ({
      hits: [
        {
          nodeId: "s1",
          kind: "session",
          field: "name",
          piece: { before: "Deep ", matched: "Session", after: "" },
        },
        {
          nodeId: "s1",
          kind: "session",
          field: "transcription",
          piece: { before: "a ", matched: "<img src=x onerror=1>", after: " today" },
        },
        {
          nodeId: "s2",
          kind: "session",
          field: "name",
          piece: { before: "Root ", matched: "Session", after: "" },
        },
      ],
      truncated: false,
    }));
    const wrapper = mountApp();
    await flushPromises();
    await openSearchView(wrapper);
    await queryInput(wrapper).setValue("session");
    await vi.advanceTimersByTimeAsync(200);
    await flushPromises();

    const groups = wrapper.findAll(".search-group");
    expect(groups.length).toBe(2);
    expect(groups[0]!.find(".search-group-name").text()).toBe("Deep Session");
    expect(groups[0]!.find(".search-group-path").text()).toBe("Folder A");
    expect(groups[1]!.find(".search-group-name").text()).toBe("Root Session");
    expect(groups[1]!.find(".search-group-path").exists()).toBe(false);
    expect(groups[0]!.findAll(".search-hit")[1]!.find(".search-field-label").text()).toBe("转写");
    expect(wrapper.find(".search-piece img").exists()).toBe(false);
    expect(wrapper.text()).toContain("<img src=x onerror=1>");
  });

  it("opens a result and reveals it in the Explorer without leaving Search", async () => {
    mockSearch(data, () => ({
      hits: [
        {
          nodeId: "s1",
          kind: "session",
          field: "name",
          piece: { before: "", matched: "Deep Session", after: "" },
        },
      ],
      truncated: false,
    }));
    const wrapper = mountApp();
    await flushPromises();
    await openSearchView(wrapper);
    await queryInput(wrapper).setValue("Deep");
    await vi.advanceTimersByTimeAsync(200);
    await flushPromises();
    await wrapper.get(".search-hit").trigger("click");
    await flushPromises();
    expect(wrapper.find(".app-header h1").text()).toBe("Deep Session");
    expect(wrapper.find(".search-view").exists()).toBe(true);

    await wrapper.get('.activity-bar [aria-label="资源管理器"]').trigger("click");
    expect(
      wrapper.findAll(".tree-session").some((row) => row.text() === "Deep Session"),
    ).toBe(true);
    expect(wrapper.get(".tree-row.focused").text()).toBe("Deep Session");
  });

  it("blocks activating a result while an operation is busy", async () => {
    mockSearch(data, () => ({
      hits: [
        {
          nodeId: "s1",
          kind: "session",
          field: "name",
          piece: { before: "", matched: "Deep Session", after: "" },
        },
      ],
      truncated: false,
    }));
    const wrapper = mountApp();
    await flushPromises();
    await openSessionNode(wrapper, "Root Session");
    await buttonWithText(wrapper, "开始录音").trigger("click");
    await flushPromises();
    await openSearchView(wrapper);
    await queryInput(wrapper).setValue("Deep");
    await vi.advanceTimersByTimeAsync(200);
    await flushPromises();

    expect(wrapper.get(".search-hit").attributes("disabled")).toBeDefined();
    await wrapper.get(".search-hit").trigger("click");
    await flushPromises();

    await wrapper.get('.activity-bar [aria-label="资源管理器"]').trigger("click");
    expect(wrapper.find(".app-header h1").text()).toBe("Root Session");
  });

  it("shows the empty prompt before typing and a no-results message after a query with no hits", async () => {
    mockSearch(data, () => ({ hits: [], truncated: false }));
    const wrapper = mountApp();
    await flushPromises();
    await openSearchView(wrapper);
    expect(wrapper.get(".search-empty").text()).toBe("输入以搜索名称、转写、摘要、材料和对话");

    await queryInput(wrapper).setValue("nothing");
    await vi.advanceTimersByTimeAsync(200);
    await flushPromises();
    expect(wrapper.get(".search-empty").text()).toBe("未找到「nothing」的结果");
  });

  it("shows the truncated footer when results are capped", async () => {
    mockSearch(data, () => ({
      hits: [
        {
          nodeId: "s2",
          kind: "session",
          field: "name",
          piece: { before: "", matched: "Root Session", after: "" },
        },
      ],
      truncated: true,
    }));
    const wrapper = mountApp();
    await flushPromises();
    await openSearchView(wrapper);
    await queryInput(wrapper).setValue("session");
    await vi.advanceTimersByTimeAsync(200);
    await flushPromises();
    expect(wrapper.get(".search-truncated").text()).toBe("结果过多，请细化搜索");
  });

  it("ArrowDown from the query focuses the first result, and Escape clears the query", async () => {
    mockSearch(data, () => ({
      hits: [
        {
          nodeId: "s2",
          kind: "session",
          field: "name",
          piece: { before: "", matched: "Root Session", after: "" },
        },
      ],
      truncated: false,
    }));
    const wrapper = mountAttached();
    await flushPromises();
    await openSearchView(wrapper);
    const input = queryInput(wrapper);
    await input.setValue("session");
    await vi.advanceTimersByTimeAsync(200);
    await flushPromises();
    await input.trigger("keydown", { key: "ArrowDown" });
    await flushPromises();
    expect(document.activeElement?.classList.contains("search-hit")).toBe(true);

    await input.trigger("keydown", { key: "Escape" });
    await flushPromises();
    expect((input.element as HTMLInputElement).value).toBe("");
  });
});
