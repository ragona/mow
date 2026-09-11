const loading = document.getElementById("loading");
const status = document.getElementById("loading-status");
const retry = document.getElementById("retry");
retry.addEventListener("click", () => location.reload());

function fail(error) {
  console.error(error);
  loading.hidden = false;
  loading.classList.add("failed");
  status.textContent = error instanceof Error ? error.message : String(error);
  retry.hidden = false;
}

window.addEventListener("unhandledrejection", event => fail(event.reason));
window.addEventListener("error", event => fail(event.error || event.message));

try {
  if (!window.isSecureContext) {
    throw new Error("M.O.W. — Mower Of Worlds needs a secure connection. Open this page over HTTPS, or use localhost for development.");
  }
  if (!navigator.gpu) {
    throw new Error("WebGPU is unavailable in this browser. Try a current Chrome, Edge, Firefox, or Safari on a supported computer, with graphics acceleration enabled.");
  }
  const { default: init, start } = await import("./lawn_orbit.js");
  await init();
  status.textContent = "Growing the garden and painting its sky…";
  await start();
  loading.hidden = true;
  document.getElementById("lawn-canvas").focus();
} catch (error) {
  fail(error);
}
