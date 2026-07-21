const buildVersion = document.currentScript?.dataset.buildVersion ?? `${Date.now()}`;
const wasmModule = await import(`./pkg/logic_conduit.js?v=${encodeURIComponent(buildVersion)}`);
const { default: init, WebHandle } = wasmModule;

const loading = document.getElementById("loading");
const canvas = document.getElementById("logic-analyzer");

try {
  const wasmUrl = new URL("./pkg/logic_conduit_bg.wasm", import.meta.url);
  wasmUrl.searchParams.set("v", buildVersion);
  await init({ module_or_path: wasmUrl });
  const handle = new WebHandle();
  await handle.start(canvas);
  window.logicConduit = handle;
  loading.remove();
} catch (error) {
  loading.textContent = "Failed to load LogicConduit";
  loading.classList.add("error");
  console.error(error);
}
