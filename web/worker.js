// Background thread for UI pages (see src/worker.rs and smPlatform.work): loads the same
// wasm and runs one job per message, in order. It has no browser APIs, only the jobs.
import init, { worker_run } from './pkg/session_manager.js';

const ready = init();
ready.then(
  () => postMessage({ ready: true }),
  (e) => postMessage({ failed: String(e?.message ?? e) }),
);

onmessage = async ({ data: { id, op, text, bytes } }) => {
  try {
    await ready;
    const out = worker_run(op, text, bytes ?? new Uint8Array());
    postMessage({ id, out }, out instanceof Uint8Array ? [out.buffer] : []);
  } catch (e) {
    postMessage({ id, error: String(e?.message ?? e) });
  }
};
