<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { i18n } from "../locales";
import { FluentButton, FluentSelect, FluentSlider, FluentSwitch } from "@platform-kit/fluent/vue";
import type { FluentSelectOption } from "@platform-kit/fluent/vue";
import type { CaptureMode, SystemAudioCapability, TranslationMode } from "../types";
import { WHISPER_LANGUAGES } from "../whisper-languages";
import { splitTranscriptSentences } from "../transcript-sentences";
import { systemAudioUnavailableLabel } from "../system-audio-reason";

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
  systemAudioCapability: SystemAudioCapability;
  language: string;
  recordingLevel: number;
  audioUrl: string | undefined;
  translationEnabled: boolean;
  translationTargetLanguage: string;
  translationMode: TranslationMode;
  // Whether a language model is configured to actually run a translation
  // request right now (distinct from the toggle itself being on).
  translatorReady: boolean;
  // Set once the current session+target-language pair's last translation
  // batch failed; the backend keeps retrying with backoff, so this is a
  // "still failing" notice rather than a terminal error.
  translationError?: string;
  // Keyed by the exact source sentence text, not by index.
  sentenceTranslations: Record<string, string>;
  translatingSentences: Set<string>;
}>();
const emit = defineEmits<{
  "upload-audio": [];
  cancel: [];
  "toggle-recording": [];
  "update:captureMode": [mode: CaptureMode];
  "update:language": [code: string];
  "update:translationEnabled": [enabled: boolean];
  "update:translationTargetLanguage": [code: string];
  "update:translationMode": [mode: TranslationMode];
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
// Reuses the same language table the transcription language picker already
// uses, rather than introducing a separate cloud-translation language list.
const translationLanguageOptions = computed<FluentSelectOption[]>(() =>
  WHISPER_LANGUAGES.map((entry) => ({ value: entry.code, label: entry.name })),
);
const translationTargetLanguageLabel = computed(
  () =>
    WHISPER_LANGUAGES.find((entry) => entry.code === props.translationTargetLanguage)?.name ??
    props.translationTargetLanguage,
);

const systemUnavailableLabel = computed(() =>
  systemAudioUnavailableLabel(props.systemAudioCapability),
);
const modeOptions = computed<FluentSelectOption[]>(() => [
  { value: "realtime", label: t("transcript.mode.realtime") },
  { value: "upload", label: t("transcript.mode.upload") },
  props.systemAudioCapability.available
    ? { value: "system", label: t("transcript.mode.system") }
    : {
        value: "system",
        label: `${t("transcript.mode.system")} — ${systemUnavailableLabel.value}`,
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
    // Removing the element from the DOM doesn't reliably stop playback, so
    // pause it explicitly before the source changes out from under it.
    audioEl.value?.pause();
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
    <div class="transcript-translate-row">
      <FluentSwitch
        :model-value="translationEnabled"
        :label="t('transcript.translate.toggle')"
        @update:model-value="(value) => emit('update:translationEnabled', value as boolean)"
      />
      <template v-if="translationEnabled">
        <FluentSelect
          class="language-select"
          :model-value="translationTargetLanguage"
          :label="t('transcript.translate.targetLanguage')"
          :options="translationLanguageOptions"
          @update:model-value="
            (value) => emit('update:translationTargetLanguage', value as string)
          "
        />
        <div class="header-switches" role="group" :aria-label="t('transcript.translate.viewMode')">
          <button
            type="button"
            :class="{ active: translationMode === 'side-by-side' }"
            @click="emit('update:translationMode', 'side-by-side')"
          >
            {{ t("transcript.translate.sideBySide") }}
          </button>
          <button
            type="button"
            :class="{ active: translationMode === 'separate' }"
            @click="emit('update:translationMode', 'separate')"
          >
            {{ t("transcript.translate.separate") }}
          </button>
        </div>
        <span v-if="!translatorReady" class="transcript-translate-status">{{
          t("transcript.translate.needsProvider")
        }}</span>
        <span v-else-if="translationError" class="transcript-translate-status">{{
          t("transcript.translate.error")
        }}</span>
      </template>
    </div>
    <div
      ref="transcriptScrollRef"
      class="transcript-content"
      :class="{ 'transcript-content-split': translationEnabled && translationMode === 'separate' }"
    >
      <template v-if="sentences.length">
        <div class="transcript-source-column">
          <div v-for="(sentence, index) in sentences" :key="index" class="transcript-sentence-block">
            <p class="transcript-sentence">{{ sentence }}</p>
            <p
              v-if="translationEnabled && translationMode === 'side-by-side' && sentenceTranslations[sentence]"
              class="transcript-translation"
            >
              {{ sentenceTranslations[sentence] }}
            </p>
            <p
              v-else-if="
                translationEnabled &&
                translationMode === 'side-by-side' &&
                translatingSentences.has(sentence)
              "
              class="transcript-translation transcript-translating"
            >
              {{ t("transcript.translate.translating") }}
            </p>
          </div>
        </div>
        <div
          v-if="translationEnabled && translationMode === 'separate'"
          class="transcript-translation-column"
        >
          <h3 class="transcript-translation-heading">{{ translationTargetLanguageLabel }}</h3>
          <p v-for="(sentence, index) in sentences" :key="index" class="transcript-translation-line">
            <template v-if="sentenceTranslations[sentence]">{{
              sentenceTranslations[sentence]
            }}</template>
            <template v-else-if="translatingSentences.has(sentence)"
              ><span class="transcript-translating">{{
                t("transcript.translate.translating")
              }}</span></template
            >
          </p>
        </div>
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
