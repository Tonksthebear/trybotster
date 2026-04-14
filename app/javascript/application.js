// Configure your import map in config/importmap.rb. Read more: https://github.com/rails/importmap-rails
import "@hotwired/turbo-rails";
import "controllers";
import "@tailwindplus/elements";
import "turbo_stream_update_attribute";
import "turbo_stream_redirect";

// Expose HubManager to the Vite/React world. The React hub-bridge reads this
// from window.__botsterHubManager to avoid crossing module graph boundaries.
import { HubManager } from "connections";
window.__botsterHubManager = HubManager;

// Expose crypto bridge and bundle parser to Vite/React world.
// The React crypto-bridge reads these to avoid crossing module graph boundaries.
import bridge from "workers/bridge";
import * as bundleModule from "matrix/bundle";
window.__botsterBridge = bridge;
window.__botsterBundle = bundleModule;

// Close mobile sidebar before Turbo caches the page so back-navigation
// doesn't restore a snapshot with the sidebar open.
// Note: el-dialog overrides <dialog>.close() with an animated version that
// resolves async, but Turbo snapshots synchronously — so bypass it entirely.
document.addEventListener("turbo:before-cache", () => {
  const dialog = document.getElementById("sidebar");
  if (!dialog?.open) return;
  dialog.removeAttribute("open");
  dialog.closest("el-dialog")?.removeAttribute("open");
});
