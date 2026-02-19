const DB_NAME = "botster_notifications"
const DB_VERSION = 1
const STORE_NAME = "notifications"
const MAX_AGE_MS = 30 * 24 * 60 * 60 * 1000 // 30 days

let dbPromise = null

function openDB() {
  if (dbPromise) return dbPromise

  dbPromise = new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, DB_VERSION)

    // NOTE: Schema must match app/views/notifications/service_worker.js.erb
    request.onupgradeneeded = (event) => {
      const db = event.target.result
      if (!db.objectStoreNames.contains(STORE_NAME)) {
        const store = db.createObjectStore(STORE_NAME, { keyPath: "id" })
        store.createIndex("createdAt", "createdAt", { unique: false })
        store.createIndex("readAt", "readAt", { unique: false })
      }
    }

    request.onsuccess = () => resolve(request.result)
    request.onerror = () => {
      dbPromise = null
      reject(request.error)
    }
  })

  return dbPromise
}

/**
 * Add a notification. Deduplicates by id (put = upsert).
 * @param {{ id: string, kind: string, hubId: string, createdAt: string }} notification
 * @returns {Promise<void>}
 */
export function add(notification) {
  return openDB().then((db) => {
    return new Promise((resolve, reject) => {
      const transaction = db.transaction(STORE_NAME, "readwrite")
      const store = transaction.objectStore(STORE_NAME)

      store.put({
        id: notification.id,
        kind: notification.kind,
        hubId: notification.hubId,
        title: notification.title || null,
        body: notification.body || null,
        url: notification.url || null,
        tag: notification.tag || null,
        readAt: null,
        createdAt: notification.createdAt || new Date().toISOString(),
      })

      transaction.oncomplete = () => resolve()
      transaction.onerror = () => reject(transaction.error)
    })
  })
}

/**
 * Get all notifications, newest first.
 * @param {number} [limit=100]
 * @returns {Promise<Array>}
 */
export function getAll(limit = 100) {
  return openDB().then((db) => {
    return new Promise((resolve, reject) => {
      const transaction = db.transaction(STORE_NAME, "readonly")
      const store = transaction.objectStore(STORE_NAME)
      const index = store.index("createdAt")
      const results = []

      const request = index.openCursor(null, "prev")
      request.onsuccess = (event) => {
        const cursor = event.target.result
        if (cursor && results.length < limit) {
          results.push(cursor.value)
          cursor.continue()
        } else {
          resolve(results)
        }
      }
      request.onerror = () => reject(request.error)
    })
  })
}

/**
 * Get count of unread notifications.
 * @returns {Promise<number>}
 */
export function getUnreadCount() {
  return openDB().then((db) => {
    return new Promise((resolve, reject) => {
      const transaction = db.transaction(STORE_NAME, "readonly")
      const store = transaction.objectStore(STORE_NAME)
      const index = store.index("readAt")

      // Count only records where readAt is null (unread)
      const request = index.count(IDBKeyRange.only(null))
      request.onsuccess = () => resolve(request.result)
      request.onerror = () => reject(request.error)
    })
  })
}

/**
 * Mark a single notification as read.
 * @param {string} id
 * @returns {Promise<void>}
 */
export function markAsRead(id) {
  return openDB().then((db) => {
    return new Promise((resolve, reject) => {
      const transaction = db.transaction(STORE_NAME, "readwrite")
      const store = transaction.objectStore(STORE_NAME)

      const request = store.get(id)
      request.onsuccess = () => {
        const record = request.result
        if (record && !record.readAt) {
          record.readAt = new Date().toISOString()
          store.put(record)
        }
      }

      transaction.oncomplete = () => resolve()
      transaction.onerror = () => reject(transaction.error)
    })
  })
}

/**
 * Mark all unread notifications as read.
 * @returns {Promise<void>}
 */
export function markAllRead() {
  return openDB().then((db) => {
    return new Promise((resolve, reject) => {
      const transaction = db.transaction(STORE_NAME, "readwrite")
      const store = transaction.objectStore(STORE_NAME)
      const now = new Date().toISOString()

      const request = store.openCursor()
      request.onsuccess = (event) => {
        const cursor = event.target.result
        if (cursor) {
          if (cursor.value.readAt === null) {
            cursor.value.readAt = now
            cursor.update(cursor.value)
          }
          cursor.continue()
        }
      }

      transaction.oncomplete = () => resolve()
      transaction.onerror = () => reject(transaction.error)
    })
  })
}

/**
 * Delete notifications older than maxAge.
 * @param {number} [maxAgeMs=MAX_AGE_MS]
 * @returns {Promise<void>}
 */
export function cleanup(maxAgeMs = MAX_AGE_MS) {
  const cutoff = new Date(Date.now() - maxAgeMs).toISOString()

  return openDB().then((db) => {
    return new Promise((resolve, reject) => {
      const transaction = db.transaction(STORE_NAME, "readwrite")
      const store = transaction.objectStore(STORE_NAME)
      const index = store.index("createdAt")
      const range = IDBKeyRange.upperBound(cutoff)

      const request = index.openCursor(range)
      request.onsuccess = (event) => {
        const cursor = event.target.result
        if (cursor) {
          cursor.delete()
          cursor.continue()
        }
      }

      transaction.oncomplete = () => resolve()
      transaction.onerror = () => reject(transaction.error)
    })
  })
}
