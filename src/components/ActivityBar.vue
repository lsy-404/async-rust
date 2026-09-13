<script setup lang="ts">
import { i18n } from "../locales";

const { t } = i18n.global;

defineProps<{
  activeView: "explorer" | "search";
}>();
const emit = defineEmits<{
  select: [view: "explorer" | "search"];
}>();
</script>

<template>
  <nav class="activity-bar" role="toolbar" aria-orientation="vertical" :aria-label="t('activity.title')">
    <button
      type="button"
      class="activity-btn"
      :class="{ active: activeView === 'explorer' }"
      :aria-pressed="activeView === 'explorer'"
      :aria-label="t('activity.explorer')"
      :title="t('activity.explorerTitle')"
      @click="emit('select', 'explorer')"
    >
      <svg width="18" height="18" viewBox="0 0 18 18" fill="none" aria-hidden="true">
        <path
          d="M2 4.5A1.5 1.5 0 0 1 3.5 3h3.6l1.2 1.5H14.5A1.5 1.5 0 0 1 16 6v7A1.5 1.5 0 0 1 14.5 14.5h-11A1.5 1.5 0 0 1 2 13Z"
          stroke="currentColor"
          stroke-width="1.3"
          stroke-linejoin="round"
        />
      </svg>
    </button>
    <button
      type="button"
      class="activity-btn"
      :class="{ active: activeView === 'search' }"
      :aria-pressed="activeView === 'search'"
      :aria-label="t('activity.search')"
      :title="t('activity.searchTitle')"
      @click="emit('select', 'search')"
    >
      <svg width="18" height="18" viewBox="0 0 18 18" fill="none" aria-hidden="true">
        <circle cx="7.5" cy="7.5" r="4.5" stroke="currentColor" stroke-width="1.3" />
        <path d="M11 11 15.5 15.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
      </svg>
    </button>
  </nav>
</template>
