import { createApp } from "vue";
import "@platform-kit/fluent/style.css";
import "@model-auth/vue/style.css";
import "katex/dist/katex.min.css";
import "./style.css";
import App from "./App.vue";
import { i18n } from "./locales";

createApp(App).use(i18n).mount("#app");
