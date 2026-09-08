import { createInterface } from "node:readline";
import { authorizeWorkBuddy, refreshWorkBuddy, workBuddyHeaders, WORKBUDDY_ENDPOINTS } from "@model-auth/providers/workbuddy";
import { authorizeTrae, refreshTrae, listTraeModels, streamTrae } from "@model-auth/providers/trae";

const out = (value) => process.stdout.write(JSON.stringify(value) + "\n");
const err = (message) => out({ type: "error", error: message });
const openExternal = async (url) => {
  out({ type: "url", text: url });
};
const provider = (id) => id === "workbuddy" ? "workbuddy" : id === "traecode" ? "traecode" : null;
async function run(request) {
  if (typeof request.operation === "string" && request.operation.startsWith("stt-")) {
    const stt = await import("./stt/index.mjs");
    return stt.handle(request, out);
  }
  const id = provider(request.providerId);
  if (!id) throw new Error("Unsupported OAuth provider.");
  if (request.operation === "authorize") {
    const value = id === "workbuddy" ? await authorizeWorkBuddy({ openExternal }) : await authorizeTrae({ openExternal });
    return out({ type: "result", value });
  }
  const credential = request.credential;
  if (!credential || typeof credential !== "object") throw new Error("Credential is required.");
  if (request.operation === "refresh") return out({ type: "result", value: id === "workbuddy" ? await refreshWorkBuddy(credential) : await refreshTrae(credential) });
  if (request.operation === "models") {
    if (id === "traecode") return out({ type: "result", value: await listTraeModels(credential) });
    const response = await fetch(`${WORKBUDDY_ENDPOINTS.chatBase}/models`, { headers: workBuddyHeaders(credential), redirect: "error" });
    if (!response.ok) throw new Error(`WorkBuddy models request failed (${response.status}).`);
    return out({ type: "result", value: await response.json() });
  }
  if (request.operation !== "chat" || typeof request.model !== "string" || !Array.isArray(request.messages)) throw new Error("Invalid OAuth request.");
  if (id === "traecode") {
    for await (const event of streamTrae(credential, { model: request.model, messages: request.messages })) {
      if (event.type === "text" || event.type === "reasoning") out({ type: "delta", text: event.text });
      else if (event.type === "done") out({ type: "done", text: event.finishReason });
    }
    return;
  }
  const response = await fetch(`${WORKBUDDY_ENDPOINTS.chatBase}/chat/completions`, { method: "POST", headers: { ...workBuddyHeaders(credential), "content-type": "application/json" }, body: JSON.stringify({ model: request.model, messages: request.messages, stream: true }), redirect: "error" });
  if (!response.ok || !response.body) throw new Error(`WorkBuddy chat request failed (${response.status}).`);
  for await (const chunk of response.body) {
    const text = new TextDecoder().decode(chunk);
    for (const line of text.split(/\r?\n/)) if (line.startsWith("data: ")) {
      if (line === "data: [DONE]") return out({ type: "done", text: "stop" });
      try { const delta = JSON.parse(line.slice(6))?.choices?.[0]?.delta?.content; if (typeof delta === "string") out({ type: "delta", text: delta }); } catch { throw new Error("Malformed WorkBuddy stream."); }
    }
  }
  out({ type: "done", text: "stop" });
}
const rl = createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of rl) { if (!line.trim()) continue; try { await run(JSON.parse(line)); } catch (e) { err(e instanceof Error ? e.message : "OAuth host failed."); } }
