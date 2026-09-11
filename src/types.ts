export type Theme = "system" | "light" | "dark";
export type CaptureMode = "realtime" | "upload" | "system";
export type RecordingSource = "microphone" | "systemAudio";
// `reason` is a stable machine code the caller maps through i18n, e.g.
// "unsupported-os" | "unsupported-platform"; absent when available.
export interface SystemAudioCapability {
  available: boolean;
  reason: string | null;
}
export type ToolCallStatus = "requested" | "running" | "finished" | "failed";
// One local tool invocation attached to the assistant turn that triggered it.
// Not persisted by the backend today: it only lives for the current stream.
export interface ToolCall {
  id: string;
  name: string;
  status: ToolCallStatus;
  arguments?: string;
  result?: string;
}
export interface Message {
  id: string;
  role: "user" | "assistant" | "system";
  content: string;
  // Absent for an optimistic local message not yet round-tripped through the
  // backend; empty string for one persisted before this field existed.
  createdAt?: string;
  toolCalls?: ToolCall[];
}
export interface Word {
  word: string;
  start: number;
  end: number;
}
export interface Workspace {
  id: string;
  name: string;
}
export interface Material {
  id: string;
  workspaceId: string;
  name: string;
  content: string;
  path: string;
}
export interface Session {
  id: string;
  workspaceId: string;
  title: string;
  messages: Message[];
  transcription?: string;
  summary?: string;
  summaryUpdatedAt?: string | null;
  // Null unless the bundled Whisper export carries alignment heads (it does
  // not, today) and the most recent transcribe call produced word timings.
  transcriptionWords?: Word[] | null;
}
export interface Provider {
  id: string;
  name: string;
  baseUrl: string;
  models: string[];
  hasKey: boolean;
  authMethod?: "oauth" | "api-key";
}
export interface Settings {
  providerId: string;
  model: string;
  theme: Theme;
  language: "zh" | "en";
  mainPanelRatio?: number;
  sidebarOpen?: boolean;
}
export interface SttStatus {
  ready: boolean;
  modelName: string;
  modelPath: string;
  sizeBytes: number;
}
export interface AppData {
  workspaces: Workspace[];
  sessions: Session[];
  materials: Material[];
  settings: Settings;
  providers: Provider[];
}
export interface StreamEvent {
  type: "delta" | "done" | "tool";
  text: string;
  toolCallId?: string;
  toolName?: string;
  toolStatus?: ToolCallStatus;
  toolArguments?: string;
  toolResult?: string;
}

// The wire shape per variant is exactly:
//   { type: "transcript"; sessionId: string; text: string }
//   { type: "error"; sessionId: string; text: string }
//   { type: "level"; sessionId: string; level: number }
// Loosened to one interface (rather than a discriminated union) so existing
// call sites that only branch on "error" keep type-checking; a caller that
// starts handling "level" should narrow on `type` and can tighten this.
export interface RecordingEvent {
  type: "transcript" | "error" | "level";
  sessionId: string;
  text?: string;
  level?: number;
}
