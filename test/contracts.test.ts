import { describe, expect, it } from "vitest";
import type { AppData, Settings } from "../src/types";

describe("desktop state contract", () => {
  it("keeps the local BYOK settings shape", () => {
    const settings: Settings = { providerId: "openai", model: "", transcriptionModel: "", theme: "system", language: "zh" };
    const state: AppData = { workspaces: [], sessions: [], materials: [], providers: [], settings };
    expect(state.settings.theme).toBe("system");
    expect(state.workspaces).toHaveLength(0);
  });
});
