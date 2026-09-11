// Each preparation job owns its WASM memory. Large results are transferred
// back; the Rust job drops/terminates the worker after consuming the result.
import init, { prepare_worker } from "./lawn_orbit.js";

self.onmessage = async ({ data }) => {
  try {
    await init();
    const result = prepare_worker(new Uint8Array(data));
    self.postMessage(result.buffer, [result.buffer]);
  } catch (error) {
    self.postMessage({ error: String(error) });
  }
};
