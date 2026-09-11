<script setup lang="ts">
import { ref } from "vue";
import { i18n } from "../locales";
import { FluentButton, FluentTextArea } from "@platform-kit/fluent/vue";
import WorkbenchToolCallCard from "./WorkbenchToolCallCard.vue";
import type { Message } from "../types";

const { t, locale } = i18n.global;

defineProps<{
  activeTab: "chat" | "summary";
  renderedMessages: (Message & { html: string })[];
  editingMessageId: string;
  editDraft: string;
  operationBusy: boolean;
  draft: string;
  streaming: boolean;
  hasProviderKey: boolean;
  summaryLoading: boolean;
  summaryStream: string;
  summaryUpdatedLabel: string;
  sessionSummary: string | undefined;
  summaryHtml: string;
}>();
const emit = defineEmits<{
  "update:activeTab": [value: "chat" | "summary"];
  "update:draft": [value: string];
  "update:editDraft": [value: string];
  "content-click": [event: MouseEvent];
  "copy-message": [content: string];
  "start-edit-message": [id: string, content: string];
  "cancel-edit-message": [];
  "commit-edit-message": [];
  "regenerate-message": [id: string];
  "delete-message": [id: string];
  "submit-message": [];
  cancel: [];
  "composer-enter": [];
  "composer-input": [event: Event];
  "ask-uncertain": [text: string];
  summarize: [];
}>();

const messagesRef = ref<HTMLElement>();
defineExpose({ messagesRef });

// Some IME/browser combinations only signal composition via keyCode 229, not
// event.isComposing, so composition state is tracked explicitly as well.
const composing = ref(false);
function handleComposerKeydown(event: KeyboardEvent) {
  const keyCode = (event as KeyboardEvent & { keyCode?: number }).keyCode;
  if (composing.value || event.isComposing || keyCode === 229) return;
  event.preventDefault();
  emit("composer-enter");
}

const copiedMessageId = ref("");
let copiedResetTimer: ReturnType<typeof setTimeout> | undefined;
function handleCopy(message: Message) {
  emit("copy-message", message.content);
  copiedMessageId.value = message.id;
  clearTimeout(copiedResetTimer);
  copiedResetTimer = setTimeout(() => {
    copiedMessageId.value = "";
  }, 1200);
}

function formatMessageTime(iso?: string) {
  if (!iso) return "";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  return new Intl.DateTimeFormat(locale.value, { timeStyle: "short" }).format(
    date,
  );
}

const uncertainTooltip = ref<{
  visible: boolean;
  top: number;
  left: number;
  text: string;
}>({ visible: false, top: 0, left: 0, text: "" });
let uncertainHideTimer: ReturnType<typeof setTimeout> | undefined;
function handleMessagesMouseMove(event: MouseEvent) {
  const target = (event.target as HTMLElement | null)?.closest?.(
    ".uncertain-span",
  );
  if (!target) return;
  clearTimeout(uncertainHideTimer);
  const rect = target.getBoundingClientRect();
  uncertainTooltip.value = {
    visible: true,
    top: rect.top - 8,
    left: rect.left,
    text: target.textContent ?? "",
  };
}
function scheduleHideUncertainTooltip() {
  uncertainHideTimer = setTimeout(() => {
    uncertainTooltip.value.visible = false;
  }, 150);
}
function cancelHideUncertainTooltip() {
  clearTimeout(uncertainHideTimer);
}
function handleUncertainAsk() {
  const text = uncertainTooltip.value.text.trim();
  if (!text) return;
  uncertainTooltip.value.visible = false;
  emit("ask-uncertain", text);
}
</script>

<template>
  <section class="main-workspace">
    <div class="session-tabs">
      <FluentButton
        :class="{ active: activeTab === 'chat' }"
        tone="subtle"
        @click="emit('update:activeTab', 'chat')"
        >{{ t("session.tabs.chat") }}</FluentButton
      ><FluentButton
        :class="{ active: activeTab === 'summary' }"
        tone="subtle"
        @click="emit('update:activeTab', 'summary')"
        >{{ t("session.tabs.summary") }}</FluentButton
      >
    </div>
    <template v-if="activeTab === 'chat'"
      ><div
        ref="messagesRef"
        class="messages"
        @click="emit('content-click', $event)"
        @mousemove="handleMessagesMouseMove"
        @mouseleave="scheduleHideUncertainTooltip"
      >
        <article
          v-for="message in renderedMessages"
          :key="message.id"
          :class="['message', message.role]"
        >
          <label>{{
            message.role === "user" ? t("session.you") : t("session.assistant")
          }}</label>
          <div class="message-actions">
            <button
              type="button"
              :disabled="operationBusy"
              @click="handleCopy(message)"
            >
              {{
                copiedMessageId === message.id
                  ? t("session.actions.copied")
                  : t("session.actions.copy")
              }}
            </button>
            <button
              v-if="message.role === 'user'"
              type="button"
              :disabled="operationBusy"
              @click="emit('start-edit-message', message.id, message.content)"
            >
              {{ t("session.actions.edit") }}
            </button>
            <button
              v-if="message.role === 'assistant'"
              type="button"
              :disabled="operationBusy"
              @click="emit('regenerate-message', message.id)"
            >
              {{ t("session.actions.regenerate") }}
            </button>
            <button
              type="button"
              :disabled="operationBusy"
              @click="emit('delete-message', message.id)"
            >
              {{ t("session.actions.delete") }}
            </button>
            <span v-if="formatMessageTime(message.createdAt)" class="message-time">{{
              formatMessageTime(message.createdAt)
            }}</span>
          </div>
          <div
            v-if="editingMessageId === message.id"
            class="message-edit"
          >
            <FluentTextArea
              :model-value="editDraft"
              :label="t('session.editMessageLabel')"
              @update:model-value="emit('update:editDraft', $event)"
            />
            <div class="message-edit-actions">
              <FluentButton
                tone="subtle"
                :disabled="operationBusy"
                @click="emit('cancel-edit-message')"
                >{{ t("common.cancel") }}</FluentButton
              ><FluentButton
                tone="primary"
                :disabled="operationBusy"
                @click="emit('commit-edit-message')"
                >{{ t("common.save") }}</FluentButton
              >
            </div>
          </div>
          <template v-else>
            <div v-if="message.toolCalls?.length" class="tool-call-list">
              <WorkbenchToolCallCard
                v-for="call in message.toolCalls"
                :key="call.id"
                :tool-call="call"
              />
            </div>
            <div class="message-body" v-html="message.html"></div>
          </template>
        </article>
        <div v-if="!renderedMessages.length" class="empty center">
          {{ t("session.emptyChat") }}
        </div>
        <div
          v-if="uncertainTooltip.visible"
          class="uncertain-tooltip"
          :style="{ top: uncertainTooltip.top + 'px', left: uncertainTooltip.left + 'px' }"
          @mouseenter="cancelHideUncertainTooltip"
          @mouseleave="scheduleHideUncertainTooltip"
        >
          <span>{{ t("session.uncertain.label") }}</span>
          <button type="button" @click="handleUncertainAsk">
            {{ t("session.uncertain.ask") }}
          </button>
        </div>
      </div>
      <form class="composer" @submit.prevent="emit('submit-message')">
        <FluentTextArea
          :model-value="draft"
          :label="t('session.inputLabel')"
          :disabled="operationBusy"
          :placeholder="t('session.inputPlaceholder')"
          @update:model-value="emit('update:draft', $event)"
          @compositionstart="composing = true"
          @compositionend="composing = false"
          @keydown.enter.exact="handleComposerKeydown"
          @input="emit('composer-input', $event)"
        /><FluentButton v-if="streaming" tone="danger" @click="emit('cancel')"
          >{{ t("session.stop") }}</FluentButton
        ><FluentButton
          v-else
          tone="primary"
          type="submit"
          :disabled="!draft.trim() || !hasProviderKey || operationBusy"
          >{{ t("session.send") }}</FluentButton
        >
      </form></template
    >
    <section v-else class="summary-page">
      <header>
        <div>
          <h2>{{ t("summary.title") }}</h2>
          <p>{{ t("summary.description") }}</p>
        </div>
        <FluentButton
          :tone="summaryLoading ? 'danger' : 'secondary'"
          :disabled="operationBusy && !summaryLoading"
          @click="summaryLoading ? emit('cancel') : emit('summarize')"
          >{{
            summaryLoading ? t("summary.stop") : t("summary.generate")
          }}</FluentButton
        >
      </header>
      <p v-if="summaryUpdatedLabel" class="summary-meta">
        {{ t("summary.updatedAt", { time: summaryUpdatedLabel }) }}
      </p>
      <div v-if="summaryLoading && !summaryStream" class="empty">
        <span class="spinner"></span> {{ t("summary.generating") }}
      </div>
      <div
        v-else-if="sessionSummary || summaryStream"
        class="summary-content"
        v-html="summaryHtml"
        @click="emit('content-click', $event)"
      ></div>
      <div v-else class="empty">
        {{ t("summary.empty") }}
      </div>
    </section>
  </section>
</template>
