<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { i18n } from "../locales";
import { FluentButton, FluentSelect, FluentSlider } from "@platform-kit/fluent/vue";
import type { FluentSelectOption } from "@platform-kit/fluent/vue";
import type { CaptureMode } from "../types";
import { WHISPER_LANGUAGES } from "../whisper-languages";
import { splitTranscriptSentences } from "../transcript-sentences";

const { t } = i18n.global;

const props = defineProps<{
  sttReady: boolean;
  transcriptionLoading: boolean;
  operationBusy: boolean;
  hasSession: boolean;
  transcription: string | undefined;
  recordingStarting: boolean;
  recording: boolean;
  recordingSessionId: string;
  streaming: boolean;
  summaryLoading: boolean;
  captureMode: CaptureMode;
  language: string;
  recordingLevel: number;
  audioUrl: string | undefined;
}>();
const emit = defineEmits<{
  "upload-audio": [];
  cancel: [];
  "toggle-recording": [];
  "update:captureMode": [mode: CaptureMode];
  "update:language": [code: string];
}>();

const transcriptScrollRef = ref<HTMLElement>();
defineExpose({ transcriptScrollRef });

const sentences = computed(() =>
  splitTranscriptSentences(props.transcription ?? ""),
);

const languageOptions = computed<FluentSelectOption[]>(() => [
  { value: "", label: t("transcript.language.auto") },
  ...WHISPER_LANGUAGES.map((entry) => ({ value: entry.code, label: entry.name })),
]);

const modeOptions = computed<FluentSelectOption[]>(() => [
  { value: "realtime", label: t("transcript.mode.realtime") },
  { value: "upload", label: t("transcript.mode.upload") },
  {
    value: "system",
    label: `${t("transcript.mode.system")} — ${t("transcript.mode.systemUnavailable")}`,
    disabled: true,
  },
]);

const primaryLabel = computed(() =>
  props.captureMode === "upload"
    ? t("transcript.import")
    : props.recording
      ? t("transcript.recording.stop")
      : t("transcript.recording.start"),
);
const primaryBusy = computed(() =>
  props.captureMode === "upload" ? props.transcriptionLoading : props.recordingStarting,
);
const primaryDisabled = computed(() =>
  props.captureMode === "upload"
    ? !props.hasSession || props.operationBusy
    : !props.hasSession ||
      props.streaming ||
      props.summaryLoading ||
      props.transcriptionLoading,
);
function handlePrimaryAction() {
  if (props.captureMode === "upload") emit("upload-audio");
  else emit("toggle-recording");
}

// A short rolling history turns the single latest level into readable bars;
// it resets whenever recording stops so a new session starts from silence.
const LEVEL_BAR_COUNT = 24;
const levelBars = ref<number[]>(Array(LEVEL_BAR_COUNT).fill(0));
watch(
  () => props.recordingLevel,
  (level) => {
    if (!props.recording) return;
    levelBars.value = [...levelBars.value.slice(1), level];
  },
);
watch(
  () => props.recording,
  (isRecording) => {
    if (!isRecording) levelBars.value = Array(LEVEL_BAR_COUNT).fill(0);
  },
);

const audioEl = ref<HTMLAudioElement>();
const isPlaying = ref(false);
const currentTime = ref(0);
const duration = ref(0);
watch(
  () => props.audioUrl,
  () => {
    isPlaying.value = false;
    currentTime.value = 0;
    duration.value = 0;
  },
);
function togglePlayback() {
  const el = audioEl.value;
  if (!el) return;
  if (isPlaying.value) el.pause();
  else void el.play();
}
function onTimeUpdate() {
  currentTime.value = audioEl.value?.currentTime ?? 0;
}
function onLoadedMetadata() {
  duration.value = audioEl.value?.duration ?? 0;
}
function onSeek(value: number) {
  if (audioEl.value) audioEl.value.currentTime = value;
  currentTime.value = value;
}
function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "0:00";
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${s.toString().padStart(2, "0")}`;
}
</script>

<template>
  <aside class="transcript-panel">
    <header>
      <div>
        <h2>{{ t("transcript.title") }}</h2>
        <p>
          {{
            sttReady ? t("transcript.readyHint") : t("transcript.notReadyHint")
          }}
        </p>
      </div>
      <FluentSelect
        class="language-select"
        :model-value="language"
        :label="t('transcript.language.label')"
        :options="languageOptions"
        :disabled="!hasSession || recording || recordingStarting"
        @update:model-value="(value) => emit('update:language', value as string)"
      />
    </header>
    <div ref="transcriptScrollRef" class="transcript-content">
      <template v-if="sentences.length">
        <p v-for="(sentence, index) in sentences" :key="index" class="transcript-sentence">
          {{ sentence }}
        </p>
      </template>
      <div v-else class="empty transcript-empty">
        <svg
          width="32"
          height="32"
          viewBox="0 0 24 24"
          fill="none"
          aria-hidden="true"
        >
          <path
            d="M12 15a3 3 0 0 0 3-3V6a3 3 0 1 0-6 0v6a3 3 0 0 0 3 3Z"
            stroke="currentColor"
            stroke-width="1.4"
          /><path
            d="M5 11v1a7 7 0 0 0 14 0v-1M12 19v3"
            stroke="currentColor"
            stroke-width="1.4"
            stroke-linecap="round"
          />
        </svg>
        <p>{{ t("transcript.empty") }}</p>
      </div>
    </div>
    <div class="transcript-footer">
      <div v-if="audioUrl" class="playback-bar">
        <audio
          ref="audioEl"
          :src="audioUrl"
          @play="isPlaying = true"
          @pause="isPlaying = false"
          @ended="isPlaying = false"
          @timeupdate="onTimeUpdate"
          @loadedmetadata="onLoadedMetadata"
        />
        <FluentButton tone="subtle" @click="togglePlayback">{{
          isPlaying ? t("transcript.playback.pause") : t("transcript.playback.play")
        }}</FluentButton>
        <span class="playback-time">{{ formatTime(currentTime) }}</span>
        <FluentSlider
          class="playback-slider"
          :model-value="currentTime"
          :min="0"
          :max="duration || 0"
          :step="0.1"
          @update:model-value="onSeek"
        />
        <span class="playback-time">{{ formatTime(duration) }}</span>
      </div>
      <footer class="recording-bar">
        <FluentSelect
          class="mode-select"
          :model-value="captureMode"
          :label="t('transcript.mode.label')"
          :options="modeOptions"
          :disabled="!hasSession || operationBusy"
          @update:model-value="(value) => emit('update:captureMode', value as CaptureMode)"
        />
        <span class="recording-state">{{
          recordingStarting
            ? t("transcript.recording.loadingModel")
            : recording
              ? t("transcript.recording.active")
              : transcriptionLoading
                ? t("transcript.recording.transcribing")
                : t("transcript.recording.idle")
        }}</span>
        <span v-if="recording" class="level-meter" aria-hidden="true"
          ><span
            v-for="(level, index) in levelBars"
            :key="index"
            :style="{ height: `${4 + Math.max(0, Math.min(1, level)) * 16}px` }"
        /></span>
        <FluentButton
          v-if="transcriptionLoading && !recordingSessionId"
          tone="danger"
          @click="emit('cancel')"
          >{{ t("transcript.recording.cancel") }}</FluentButton
        ><FluentButton
          :tone="recording ? 'danger' : 'primary'"
          :busy="primaryBusy"
          :disabled="primaryDisabled"
          @click="handlePrimaryAction"
          >{{ primaryLabel }}</FluentButton
        >
      </footer>
    </div>
  </aside>
</template>
