import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  ModelAuthDialog,
  ModelConnectionPanel,
  type ModelAuthAction,
  type ModelAuthState,
} from "@model-auth/vue";
import ModelConnections from "../src/components/ModelConnections.vue";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke,
  Channel: class {
    onmessage?: (event: unknown) => void;
  },
}));

const initialState = (): ModelAuthState => ({
  providers: [
    {
      id: "fixture",
      name: "Fixture",
      description: "Native provider",
      authMethods: ["oauth", "api-key"],
      available: true,
      oauthEnabled: true,
      loadStrategy: "weighted-round-robin",
      models: ["classroom-a", "classroom-b"],
      apiKeyModels: ["classroom-a"],
      oauthModels: ["classroom-b"],
      apiKeyCredentials: [
        {
          id: "key-first",
          label: "First key",
          healthy: true,
          enabled: true,
          weight: 2,
          models: ["classroom-a"],
        },
        {
          id: "key-second",
          label: "Second key",
          healthy: false,
          enabled: false,
          weight: 4,
          models: ["classroom-a"],
          cooldownUntilUtc: "2030-01-01T00:00:00Z",
        },
      ],
      oauthCredentials: [
        {
          id: "account-first",
          label: "Class account",
          account: "fixture-account",
          healthy: true,
          enabled: true,
          weight: 3,
          models: ["classroom-b"],
        },
      ],
    },
  ],
  model: { providerId: "fixture", model: "classroom-a" },
  catalogStatus: { state: "error", source: "cached", error: "offline" },
});

let wrapper: VueWrapper;
let state: ModelAuthState;
let refreshWorkbench: ReturnType<typeof vi.fn>;
async function mountConnections() {
  wrapper = mount(ModelConnections, {
    props: { theme: "dark", refreshWorkbench },
    attachTo: document.body,
  });
  await flushPromises();
  return wrapper;
}
async function manage(method: "api-key" | "oauth") {
  await wrapper
    .get(`[data-auth-method="${method}"] [data-part="view-connection"]`)
    .trigger("click");
  await flushPromises();
}
function actions(): ModelAuthAction[] {
  return invoke.mock.calls
    .filter(([command]) => command === "model_auth_action")
    .map(([, args]) => args.action);
}
function button(text: string) {
  const result = wrapper.findAll("button").find((item) => item.text() === text);
  if (!result) throw new Error(`Missing button ${text}`);
  return result;
}

beforeEach(() => {
  state = initialState();
  refreshWorkbench = vi.fn(async () => undefined);
  invoke.mockReset();
  invoke.mockImplementation(async (command) =>
    command === "get_auth_state" ? structuredClone(state) : undefined,
  );
  vi.stubGlobal("matchMedia", () => ({ matches: true }));
  HTMLDialogElement.prototype.showModal = function () {
    this.open = true;
  };
  HTMLDialogElement.prototype.close = function () {
    this.open = false;
  };
});
afterEach(() => {
  wrapper?.unmount();
  vi.unstubAllGlobals();
});

describe("complete shared model authentication UI", () => {
  it("renders the native credential metadata and opens the matching real kit detail", async () => {
    await mountConnections();
    const panel = wrapper.getComponent(ModelConnectionPanel);
    expect(panel.props("providers")).toEqual(state.providers);
    expect(panel.props("model")).toEqual(state.model);
    expect(panel.props("theme")).toBe("dark");
    expect(wrapper.findAll('[data-part="connection-account"]')).toHaveLength(3);
    expect(wrapper.text()).toContain("Second key");
    expect(wrapper.text()).toContain("加权轮询");
    await manage("oauth");
    const dialog = wrapper.getComponent(ModelAuthDialog);
    expect(dialog.props("initialConnection")).toEqual({
      providerId: "fixture",
      method: "oauth",
    });
    expect(dialog.props("catalogStatus")).toEqual(state.catalogStatus);
    expect(wrapper.get('[data-part="connection-info"]').text()).toContain(
      "fixture-account",
    );
    expect(wrapper.find('[data-part="method-list"]').exists()).toBe(false);
    await wrapper.get('[data-part="close"]').trigger("click");
    await flushPromises();
    await wrapper.get('[data-part="add-connection"]').trigger("click");
    await flushPromises();
    expect(dialog.props("initialConnection")).toBeNull();
    expect(wrapper.get('[data-part="method-list"]').text()).toContain(
      "API Key",
    );
  });

  it("uses real credential controls, native strategy action and confirmed individual removal", async () => {
    await mountConnections();
    await manage("api-key");
    await wrapper.get('input[aria-label="启用 First key"]').setValue(false);
    await flushPromises();
    await wrapper.get('input[aria-label="权重 Second key"]').setValue("7");
    await flushPromises();
    const row = wrapper.findAll('[data-part="api-key-credential"]')[1]!;
    await row.get("button[data-confirmed]").trigger("click");
    expect(actions()).toHaveLength(2);
    await row.get("button[data-confirmed]").trigger("click");
    await flushPromises();
    await wrapper.get('[aria-label="负载策略"]').trigger("click");
    await flushPromises();
    await wrapper.get('[role="option"]').trigger("click");
    await flushPromises();
    expect(actions()).toEqual([
      {
        type: "update-credential",
        payload: {
          providerId: "fixture",
          credentialId: "key-first",
          enabled: false,
          weight: 2,
        },
      },
      {
        type: "update-credential",
        payload: {
          providerId: "fixture",
          credentialId: "key-second",
          enabled: false,
          weight: 7,
        },
      },
      {
        type: "remove-credential",
        providerId: "fixture",
        credentialId: "key-second",
        authMethod: "api-key",
      },
      {
        type: "update-strategy",
        payload: { providerId: "fixture", strategy: "round-robin" },
      },
    ]);
    expect(refreshWorkbench).toHaveBeenCalledTimes(4);
  });

  it("submits a labeled API key through the real form and clears its input", async () => {
    await mountConnections();
    await wrapper.get('[data-part="add-connection"]').trigger("click");
    await wrapper.get('[data-part="method-api-key"]').trigger("click");
    await wrapper.get('[part="provider-row"]').trigger("click");
    await wrapper.get('input[aria-label="标签（可选）"]').setValue("Extra key");
    await wrapper.get('input[aria-label="API Key"]').setValue("fixture-secret");
    expect(wrapper.text()).toContain("本机应用数据目录的文件");
    await wrapper.get('[data-part="api-key-form"]').trigger("submit");
    expect(
      (wrapper.get('input[aria-label="API Key"]').element as HTMLInputElement)
        .value,
    ).toBe("");
    await flushPromises();
    expect(actions()).toEqual([
      {
        type: "add-api-key",
        payload: {
          providerId: "fixture",
          label: "Extra key",
          apiKey: "fixture-secret",
        },
      },
    ]);
  });

  it("forwards OAuth provider toggles, refresh and model selection using the kit host listeners", async () => {
    await mountConnections();
    await wrapper.get('[data-part="refresh-connections"]').trigger("click");
    await flushPromises();
    await manage("oauth");
    await wrapper
      .get('input[aria-label="启用此提供商的 OAuth"]')
      .setValue(false);
    await flushPromises();
    const row = wrapper.get('[data-part="oauth-credential"]');
    await row.get("button[data-confirmed]").trigger("click");
    await row.get("button[data-confirmed]").trigger("click");
    await flushPromises();
    wrapper
      .getComponent(ModelAuthDialog)
      .vm.$emit("select-model", {
        providerId: "fixture",
        model: "classroom-b",
      });
    await flushPromises();
    expect(actions()).toEqual([
      { type: "refresh-catalog" },
      {
        type: "update-provider",
        payload: { providerId: "fixture", oauthEnabled: false },
      },
      {
        type: "remove-credential",
        providerId: "fixture",
        credentialId: "account-first",
        authMethod: "oauth",
      },
      {
        type: "select-model",
        payload: { providerId: "fixture", model: "classroom-b" },
      },
    ]);
    expect(wrapper.getComponent(ModelAuthDialog).props("open")).toBe(false);
  });

  it.each(["close", "cancel", "unmount"])(
    "cancels the pending native reconnect on %s",
    async (mode) => {
      let rejectAction: (error: Error) => void = () => undefined;
      invoke.mockImplementation((command, args) => {
        if (command === "get_auth_state")
          return Promise.resolve(structuredClone(state));
        if (command === "model_auth_action") {
          args.onEvent.onmessage({
            type: "status",
            text: "等待浏览器完成授权",
          });
          return new Promise((_, reject) => {
            rejectAction = reject;
          });
        }
        if (command === "cancel_model_auth")
          rejectAction(new Error("cancelled"));
        return Promise.resolve();
      });
      await mountConnections();
      await manage("oauth");
      await button("重连").trigger("click");
      await flushPromises();
      expect(wrapper.text()).toContain("等待浏览器完成授权");
      expect(actions()).toEqual([
        {
          type: "authorize-oauth",
          providerId: "fixture",
          credentialId: "account-first",
        },
      ]);
      const operationId = invoke.mock.calls.find(
        ([command]) => command === "model_auth_action",
      )![1].operationId;
      if (mode === "unmount") wrapper.unmount();
      else if (mode === "cancel") await button("取消授权").trigger("click");
      else await wrapper.get('[data-part="close"]').trigger("click");
      await flushPromises();
      expect(invoke).toHaveBeenCalledWith("cancel_model_auth", { operationId });
      expect(refreshWorkbench).not.toHaveBeenCalled();
    },
  );

  it("opens a new OAuth authorization and retains existing native state on failure", async () => {
    await mountConnections();
    await wrapper.get('[data-part="add-connection"]').trigger("click");
    await wrapper.get('[data-part="method-oauth"]').trigger("click");
    await wrapper.get('[part="provider-row"]').trigger("click");
    invoke.mockImplementation(async (command) => {
      if (command === "model_auth_action") throw new Error("provider rejected");
      return structuredClone(state);
    });
    await button("添加 OAuth 账号").trigger("click");
    await flushPromises();
    expect(actions()).toEqual([
      { type: "authorize-oauth", providerId: "fixture" },
    ]);
    expect(
      wrapper.getComponent(ModelConnectionPanel).props("providers"),
    ).toEqual(state.providers);
    expect(wrapper.text()).toContain("操作未完成，请重试。");
    expect(wrapper.text()).not.toContain("取消授权");
  });
});
