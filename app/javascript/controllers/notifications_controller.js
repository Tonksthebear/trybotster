import { Controller } from "@hotwired/stimulus"
import * as NotificationStore from "notifications/store"
import { displayText, formatTime, notificationUrl } from "notifications/renderer"

/**
 * Notifications Controller
 *
 * Manages notification bell badge (sidebar) and notification list (page).
 * Reads from IndexedDB — all data is client-side only.
 * The service worker writes to IndexedDB when web push arrives.
 */
export default class extends Controller {
  static targets = ["badge", "count", "list", "empty", "template"]
  static values = { page: { type: Boolean, default: false } }

  connect() {
    this.#loadUnreadCount()

    if (this.pageValue) {
      this.#renderList()
    }

    this.#maybeCleanup()
  }

  markAsRead(event) {
    const article = event.target.closest("[data-notification-id]")
    const id = article?.dataset.notificationId
    if (!id) return

    NotificationStore.markAsRead(id).then(() => {
      this.#loadUnreadCount()
      article.classList.add("opacity-60")
      article.querySelector("[data-field='dot']")?.classList.replace("bg-primary-400", "bg-zinc-600")
      article.querySelector("[data-field='readBtn']")?.remove()
    })
  }

  markAllRead() {
    NotificationStore.markAllRead().then(() => {
      this.#loadUnreadCount()
      if (this.pageValue) this.#renderList()
    })
  }

  // ========== Private ==========

  async #loadUnreadCount() {
    try {
      const count = await NotificationStore.getUnreadCount()
      if (this.hasCountTarget) {
        this.countTarget.textContent = count > 99 ? "99+" : count
      }
      if (this.hasBadgeTarget) {
        this.badgeTarget.classList.toggle("hidden", count === 0)
      }
    } catch {
      // IndexedDB unavailable
    }
  }

  async #renderList() {
    if (!this.hasListTarget || !this.hasTemplateTarget) return

    try {
      const notifications = await NotificationStore.getAll(100)
      this.listTarget.replaceChildren()

      if (notifications.length === 0) {
        if (this.hasEmptyTarget) this.emptyTarget.classList.remove("hidden")
        return
      }

      if (this.hasEmptyTarget) this.emptyTarget.classList.add("hidden")

      const fragment = document.createDocumentFragment()
      for (const n of notifications) {
        const clone = this.templateTarget.content.cloneNode(true)
        const article = clone.querySelector("article")
        const isRead = n.readAt !== null

        article.dataset.notificationId = n.id
        if (isRead) article.classList.add("opacity-60")

        const dot = article.querySelector("[data-field='dot']")
        if (isRead && dot) dot.classList.replace("bg-primary-400", "bg-zinc-600")

        const link = article.querySelector("[data-field='link']")
        link.textContent = displayText(n.kind, n.body)
        link.href = notificationUrl(n.kind, n.hubId, n.url)

        const time = article.querySelector("[data-field='time']")
        time.textContent = formatTime(n.createdAt)

        if (isRead) {
          article.querySelector("[data-field='readBtn']")?.remove()
        }

        fragment.appendChild(clone)
      }
      this.listTarget.appendChild(fragment)
    } catch {
      // IndexedDB unavailable — empty state shown
      if (this.hasEmptyTarget) this.emptyTarget.classList.remove("hidden")
    }
  }

  #maybeCleanup() {
    if (sessionStorage.getItem("botster_notif_cleanup")) return
    sessionStorage.setItem("botster_notif_cleanup", "1")
    NotificationStore.cleanup().catch(() => {})
  }
}
