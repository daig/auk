// Loads the wasm client. window.aukDebug() reports the client's state as JSON, and
// window.aukNodeRect(pane, sf, id) where a surface's node was drawn, and window.aukHitTest(x, y)
// what a press at a point of the page would land on, for tests and for looking at a stuck page.
import init, * as auk from '/auk_web.js';

const status = document.getElementById('status');
window.aukDebug = () => {
  try {
    return auk.debug_state();
  } catch (e) {
    return JSON.stringify({ status: 'error', error: String(e) });
  }
};
window.aukNodeRect = (pane, sf, id) => {
  try {
    return auk.node_rect(pane, sf, id);
  } catch (e) {
    return 'null';
  }
};
window.aukHitTest = (x, y) => {
  try {
    return auk.hit_test(x, y);
  } catch (e) {
    return 'null';
  }
};
try {
  await init({ module_or_path: '/auk_web_bg.wasm' });
  await auk.start();
} catch (e) {
  window.aukError = String(e && e.stack || e);
  status.textContent = 'The Auk client failed to start: ' + e;
}
