import type { Provider, Settings } from "./types";

export type Invoke = <T>(
  command: string,
  args?: Record<string, unknown>,
) => Promise<T>;

export async function saveNewProvider(
  invoke: Invoke,
  provider: Provider,
  settings: Settings,
) {
  await invoke("save_provider", { provider });
  await invoke("save_settings", {
    settings: { ...settings, providerId: provider.id, model: "" },
  });
}

export async function streamCommand(
  invoke: Invoke,
  command: "chat" | "summarize",
  sessionId: string,
  onEvent: unknown,
  content?: string,
) {
  return invoke(
    command,
    command === "chat"
      ? { sessionId, content, onEvent }
      : { sessionId, onEvent },
  );
}

export async function cancelStream(invoke: Invoke, sessionId: string) {
  await invoke("cancel_generation", { sessionId });
}
