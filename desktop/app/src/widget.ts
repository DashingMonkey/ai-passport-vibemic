import { initTheme } from "./theme";

initTheme();

import { createApp } from "vue";
import Widget from "./Widget.vue";

createApp(Widget).mount("#widget");
