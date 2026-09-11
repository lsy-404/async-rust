<script setup lang="ts">
import { computed, ref } from "vue";
import { i18n } from "../locales";
import type { ToolCall } from "../types";

const { t } = i18n.global;
const props = defineProps<{
  toolCall: ToolCall;
}>();

const expanded = ref(false);
function toggle() {
  expanded.value = !expanded.value;
}

// Arguments/result arrive as raw JSON text off the wire; pretty-print when
// they parse, otherwise show the raw string rather than hiding it.
function prettyPrint(raw?: string): string {
  if (!raw) return "";
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
}
const formattedArguments = computed(() => prettyPrint(props.toolCall.arguments));
const formattedResult = computed(() => prettyPrint(props.toolCall.result));
const statusLabel = computed(() => t(`toolCall.status.${props.toolCall.status}`));
</script>

<template>
  <div class="tool-call-card" :class="`tool-call-${toolCall.status}`">
    <button
      type="button"
      class="tool-call-header"
      :aria-expanded="expanded"
      @click="toggle"
    >
      <span
        v-if="toolCall.status === 'running'"
        class="spinner tool-call-spinner"
        aria-hidden="true"
      ></span>
      <span v-else class="tool-call-status-icon" aria-hidden="true">{{
        toolCall.status === "finished"
          ? "✓"
          : toolCall.status === "failed"
            ? "!"
            : "…"
      }}</span>
      <span class="tool-call-name">{{
        t("toolCall.title", { name: toolCall.name })
      }}</span>
      <span class="tool-call-status-label">{{ statusLabel }}</span>
      <span class="tool-call-chevron" aria-hidden="true">{{
        expanded ? "▾" : "▸"
      }}</span>
    </button>
    <div v-if="expanded" class="tool-call-body">
      <div v-if="formattedArguments" class="tool-call-section">
        <div class="tool-call-section-label">{{ t("toolCall.arguments") }}</div>
        <pre>{{ formattedArguments }}</pre>
      </div>
      <div v-if="formattedResult" class="tool-call-section">
        <div class="tool-call-section-label">{{ t("toolCall.result") }}</div>
        <pre>{{ formattedResult }}</pre>
      </div>
    </div>
  </div>
</template>
