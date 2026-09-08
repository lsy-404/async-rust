import { describe, expect, it, vi } from "vitest";
import {
  cancelStream,
  removeProviderKey,
  saveNewProvider,
  streamCommand,
} from "../src/workbench-commands";
import type { Provider, Settings } from "../src/types";

const provider: Provider = {
  id: "classroom",
  name: "Classroom",
  baseUrl: "http://localhost/v1",
  models: [],
  hasKey: true,
};
const settings: Settings = {
  providerId: "openai",
  model: "",
  transcriptionModel: "",
  theme: "system",
  language: "zh",
};

describe("workbench IPC operations", () => {
  it("clears only the API key with the backend empty-string contract", async () => {
    const invoke = vi.fn().mockResolvedValue(undefined);
    await removeProviderKey(invoke, provider);
    expect(invoke).toHaveBeenCalledWith("save_provider", {
      provider,
      apiKey: "",
    });
  });
  it("persists a newly selected provider before reload", async () => {
    const invoke = vi.fn().mockResolvedValue(undefined);
    await saveNewProvider(invoke, provider, settings);
    expect(invoke.mock.calls).toEqual([
      ["save_provider", { provider, apiKey: null }],
      ["save_settings", { settings: { ...settings, providerId: "classroom" } }],
    ]);
  });
  it("captures the supplied session for streaming and cancellation", async () => {
    const invoke = vi
      .fn()
      .mockRejectedValueOnce(new Error("network down"))
      .mockResolvedValue(undefined);
    await expect(
      streamCommand(invoke, "chat", "session-a", {}, "hello"),
    ).rejects.toThrow("network down");
    await cancelStream(invoke, "session-a");
    expect(invoke.mock.calls[0]?.[1]).toMatchObject({
      sessionId: "session-a",
      content: "hello",
    });
    expect(invoke.mock.calls[1]).toEqual([
      "cancel_generation",
      { sessionId: "session-a" },
    ]);
  });
  it("starts summary with no chat content", async () => {
    const invoke = vi.fn().mockResolvedValue(undefined);
    await streamCommand(invoke, "summarize", "session-b", {});
    expect(invoke).toHaveBeenCalledWith("summarize", {
      sessionId: "session-b",
      onEvent: {},
    });
  });
});
