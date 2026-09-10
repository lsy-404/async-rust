<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { Channel, invoke } from "@tauri-apps/api/core";
import { i18n } from "../locales";
import {
  ModelAuthDialog,
  ModelConnectionPanel,
  useModelAuth,
  type ModelAuthState,
  type ModelConnectionTarget,
} from "@model-auth/vue";
import { FluentButton } from "@platform-kit/fluent/vue";
import type { Theme } from "../types";

const props = defineProps<{
  theme: Theme;
  refreshWorkbench: () => Promise<void>;
}>();
const { t } = i18n.global;
const initialConnection = ref<ModelConnectionTarget | null>(null);
const oauthProgress = ref("");
const oauthActive = ref(false);
let cancelActive: (() => void) | undefined;
const messages = computed(() => ({
  keyHint: t("modelConnections.keyHint"),
}));
const auth = useModelAuth({
  getState: () => invoke<ModelAuthState>("get_auth_state"),
  async execute(action, { signal }) {
    const operationId = crypto.randomUUID();
    const onEvent = new Channel<{ type: "status"; text: string }>();
    onEvent.onmessage = (event) => {
      oauthProgress.value = event.text;
    };
    const cancel = () => {
      void invoke("cancel_model_auth", { operationId }).catch(() => undefined);
    };
    cancelActive = cancel;
    signal.addEventListener("abort", cancel, { once: true });
    oauthActive.value = action.type === "authorize-oauth";
    oauthProgress.value = t("modelConnections.openingAuth");
    try {
      if (signal.aborted) return;
      await invoke("model_auth_action", { action, onEvent, operationId });
      await props.refreshWorkbench();
    } finally {
      signal.removeEventListener("abort", cancel);
      cancelActive = undefined;
      oauthActive.value = false;
      oauthProgress.value = "";
    }
  },
});

function manage(target: ModelConnectionTarget) {
  initialConnection.value = target;
  auth.open.value = true;
}
function add() {
  initialConnection.value = null;
  auth.open.value = true;
}
function refresh() {
  return auth.refresh();
}
onMounted(refresh);
onBeforeUnmount(() => cancelActive?.());
defineExpose({ refresh });
</script>

<template>
  <ModelConnectionPanel
    :providers="auth.state.value.providers"
    :model="auth.state.value.model"
    :busy="auth.busy.value"
    :error="auth.error.value"
    :theme="theme"
    :messages="messages"
    @manage="manage"
    @add="add"
    @refresh="auth.listeners['refresh-catalog']"
  />
  <ModelAuthDialog
    v-bind="auth.props.value"
    :theme="theme"
    :messages="messages"
    :initial-connection="initialConnection"
    v-on="auth.listeners"
  >
    <template v-if="oauthActive" #footer>
      <div role="status">{{ oauthProgress || t("modelConnections.connecting") }}</div>
      <FluentButton tone="danger" @click="auth.listeners.close">
        {{ t("modelConnections.cancelAuth") }}
      </FluentButton>
    </template>
  </ModelAuthDialog>
</template>
