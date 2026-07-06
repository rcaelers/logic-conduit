import init, { WebHandle } from "./pkg/dsl_ui.js";

const loading = document.getElementById("loading");
const canvas = document.getElementById("dsl-ui");

try {
  await init();
  const handle = new WebHandle();
  await handle.start(canvas);
  window.dslUi = handle;
  loading.remove();
} catch (error) {
  loading.textContent = "Failed to load DSL Pipeline Editor";
  loading.classList.add("error");
  console.error(error);
}

