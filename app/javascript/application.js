// Configure your import map in config/importmap.rb. Read more: https://github.com/rails/importmap-rails
import "@hotwired/turbo-rails";
import "controllers";
import "@tailwindplus/elements";
import "turbo_stream_update_attribute";
import "turbo_stream_redirect";

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
