<script setup lang="ts">
import { ref } from "vue";
import { i18n } from "../locales";
import { FluentButton } from "@platform-kit/fluent/vue";

const { t } = i18n.global;

defineProps<{
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
}>();
defineEmits<{
  "upload-audio": [];
  cancel: [];
  "toggle-recording": [];
}>();

const transcriptScrollRef = ref<HTMLElement>();
defineExpose({ transcriptScrollRef });
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
      <FluentButton
        tone="subtle"
        :busy="transcriptionLoading"
        :disabled="!hasSession || operationBusy"
        @click="$emit('upload-audio')"
        >{{ t("transcript.import") }}</FluentButton
      >
    </header>
    <div ref="transcriptScrollRef" class="transcript-content">
      <p v-if="transcription">{{ transcription }}</p>
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
    <footer class="recording-bar">
      <span class="recording-state">{{
        recordingStarting
          ? t("transcript.recording.loadingModel")
          : recording
            ? t("transcript.recording.active")
            : transcriptionLoading
              ? t("transcript.recording.transcribing")
              : t("transcript.recording.idle")
      }}</span
      ><FluentButton
        v-if="transcriptionLoading && !recordingSessionId"
        tone="danger"
        @click="$emit('cancel')"
        >{{ t("transcript.recording.cancel") }}</FluentButton
      ><FluentButton
        :tone="recording ? 'danger' : 'primary'"
        :busy="recordingStarting"
        :disabled="
          !hasSession ||
          streaming ||
          summaryLoading ||
          transcriptionLoading
        "
        @click="$emit('toggle-recording')"
        >{{ recording ? t("transcript.recording.stop") : t("transcript.recording.start") }}</FluentButton
      >
    </footer>
  </aside>
</template>
