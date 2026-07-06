const buildVersion = document.currentScript?.dataset.buildVersion ?? `${Date.now()}`;
const wasmModule = await import(`./pkg/dsl_ui.js?v=${encodeURIComponent(buildVersion)}`);
const { default: init, WebHandle } = wasmModule;

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
