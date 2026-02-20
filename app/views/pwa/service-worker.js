// Botster PWA Service Worker
// Handles web push notifications.
//
// Payload format: Declarative Web Push (RFC 8030 / Safari 18.4+)
// On Safari 18.4+, the OS handles notification display and navigation
// directly — this service worker is not invoked for push.
// On Chrome/Firefox, this service worker fires as the fallback.

const DISPLAY_TEXT = {
  agent_alert: "Your attention is needed",
  test: "Test notification — push is working!",
};

self.addEventListener("install", (event) => {
  event.waitUntil(self.skipWaiting());
});

self.addEventListener("activate", (event) => {
  event.waitUntil(self.clients.claim());
});

self.addEventListener("push", (event) => {
  const payload = event.data?.json() ?? {};
  const n = payload.notification || {};
  const data = n.data || {};

  const title = n.title || "Botster";
  const body = n.body || DISPLAY_TEXT[data.kind] || "You have a new notification";
  const url = data.url || (data.hubId ? `/hubs/${data.hubId}` : "/");
  const icon = n.icon || "/icon.png";

  const options = { body, icon, data: { url } };
  if (n.tag) options.tag = n.tag;

  // Set app badge count (mirrors Declarative Web Push app_badge for Chrome/Firefox)
  const badge = payload.app_badge;
  const badgePromise = (typeof badge === "number" && navigator.setAppBadge)
    ? navigator.setAppBadge(badge)
    : Promise.resolve();

  event.waitUntil(
    Promise.all([
      self.registration.showNotification(title, options),
      badgePromise,
    ])
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();

  const url = event.notification.data?.url || "/";

  event.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((clients) => {
      for (const client of clients) {
        if (client.url.includes(self.location.origin) && "focus" in client) {
          client.navigate(url);
          return client.focus();
        }
      }
      return self.clients.openWindow(url);
    })
  );
});
