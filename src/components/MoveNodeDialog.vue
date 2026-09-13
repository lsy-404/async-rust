<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { FluentButton, FluentDialog, FluentSelect } from "@platform-kit/fluent/vue";
import type { FluentSelectOption } from "@platform-kit/fluent/vue";
import { i18n } from "../locales";
import type { Node } from "../types";
import { moveDestinations } from "../explorer/tree";

const { t } = i18n.global;

// The keyboard/accessibility alternative to drag-and-drop: same move_node
// command, reached through a dialog instead of a native drag gesture.
const props = defineProps<{
  nodes: Node[];
  nodeId: string;
  operationBusy: boolean;
}>();
const emit = defineEmits<{
  cancel: [];
  confirm: [parentId: string | null];
}>();

// FluentSelectOption values are non-nullable strings; this stands in for the
// root option (parentId: null) and is never a real node id.
const ROOT_VALUE = "__root__";

const node = computed(() => props.nodes.find((item) => item.id === props.nodeId));
const destinations = computed(() =>
  moveDestinations(props.nodes, props.nodeId, t("explorer.moveRoot")),
);
const options = computed<FluentSelectOption[]>(() =>
  destinations.value.map((destination) => ({
    value: destination.parentId ?? ROOT_VALUE,
    label: destination.label,
  })),
);
const selected = ref("");
watch(
  () => props.nodeId,
  () => {
    selected.value = "";
  },
  { immediate: true },
);
function confirm() {
  if (!selected.value || props.operationBusy) return;
  emit("confirm", selected.value === ROOT_VALUE ? null : selected.value);
}
</script>

<template>
  <FluentDialog :open="true" :label="t('explorer.moveDialogTitle', { name: node?.name ?? '' })">
    <template #title>
      <h2>{{ t("explorer.moveDialogTitle", { name: node?.name ?? "" }) }}</h2>
    </template>
    <template #default>
      <FluentSelect
        :model-value="selected"
        :label="t('explorer.moveTo')"
        :options="options"
        @update:model-value="(value) => (selected = value as string)"
      />
    </template>
    <template #footer>
      <FluentButton tone="subtle" @click="emit('cancel')">{{ t("common.cancel") }}</FluentButton>
      <FluentButton tone="primary" :disabled="operationBusy || !selected" @click="confirm">{{
        t("explorer.moveConfirm")
      }}</FluentButton>
    </template>
  </FluentDialog>
</template>
