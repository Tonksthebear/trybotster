// Botster PWA Service Worker
// Handles web push notifications and stores them in IndexedDB.
//
// Payload format: Declarative Web Push (RFC 8030 / Safari 18.4+)
// On Safari 18.4+, the OS handles notification display and navigation
// directly — this service worker is not invoked for push.
// On Chrome/Firefox, this service worker fires as the fallback.
//
// NOTE: IndexedDB schema (DB_NAME, DB_VERSION, STORE_NAME, keyPath, indexes)
// must match app/javascript/notifications/store.js exactly.

const DB_NAME = "botster_notifications";
const DB_VERSION = 1;
const STORE_NAME = "notifications";

function openDB() {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, DB_VERSION);

    request.onupgradeneeded = (event) => {
      const db = event.target.result;
      if (!db.objectStoreNames.contains(STORE_NAME)) {
        const store = db.createObjectStore(STORE_NAME, { keyPath: "id" });
        store.createIndex("createdAt", "createdAt", { unique: false });
        store.createIndex("readAt", "readAt", { unique: false });
      }
    };

    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

function storeNotification(data) {
  return openDB().then((db) => {
    return new Promise((resolve, reject) => {
      const tx = db.transaction(STORE_NAME, "readwrite");
      const store = tx.objectStore(STORE_NAME);

      store.put({
        id: data.id,
        kind: data.kind,
        hubId: data.hubId,
        title: data.title || null,
        body: data.body || null,
        url: data.url || null,
        tag: data.tag || null,
        readAt: null,
        createdAt: data.createdAt || new Date().toISOString(),
      });

      tx.oncomplete = () => resolve();
      tx.onerror = () => reject(tx.error);
    });
  });
}

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
  const url = data.url || (data.hubId ? `/hubs/${data.hubId}` : "/notifications");
  const icon = n.icon || "/icon.png";

  const options = { body, icon, data: { url } };
  if (n.tag) options.tag = n.tag;

  // Merge notification-level fields into data for IndexedDB storage.
  // title/body/tag live at the notification level in the declarative payload
  // but storeNotification expects them on the data object.
  const record = { ...data, title: n.title, body: n.body, tag: n.tag };

  event.waitUntil(
    Promise.all([
      storeNotification(record),
      self.registration.showNotification(title, options),
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
