export type Theme = "system" | "light" | "dark";
export interface Message {
  id: string;
  role: "user" | "assistant" | "system";
  content: string;
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
  type: "delta" | "done";
  text: string;
}

export interface RecordingEvent {
  type: "transcript" | "error";
  sessionId: string;
  text: string;
}
