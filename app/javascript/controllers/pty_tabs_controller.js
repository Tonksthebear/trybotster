import { Controller } from "@hotwired/stimulus";
import { ConnectionManager, HubConnection } from "connections";

/**
 * Dynamic PTY Tab Controller
 *
 * Renders PTY session tabs based on the sessions[] array from agent info.
 * Also controls Preview button visibility — only shown when the current
 * PTY session has port_forward enabled.
 */
export default class extends Controller {
  static targets = ["tabBar", "preview", "addSession"];

  static values = {
    hubId: String,
    agentIndex: Number,
    ptyIndex: { type: Number, default: 0 },
  };

  #hubConn = null;
  #unsubscribers = [];
  #disconnected = false;
  #currentAgentId = null;

  connect() {
    this.#disconnected = false;
    this.#initConnection();
  }

  disconnect() {
    this.#disconnected = true;
    this.#unsubscribers.forEach((unsub) => unsub());
    this.#unsubscribers = [];
    this.#hubConn?.release();
    this.#hubConn = null;
  }

  // Action: set agent ID on the add-session modal before it opens
  prepareAddSession() {
    if (!this.#currentAgentId) return;
    const modal = document.getElementById("add-session-modal");
    const controller = modal?.querySelector(
      "[data-controller='add-session-modal']",
    );
    if (controller) controller.dataset.agentId = this.#currentAgentId;
  }

  // Action: remove a session by pty index (from close button data attribute)
  removeSession(event) {
    event.preventDefault();
    event.stopPropagation();
    const btn = event.currentTarget;
    const ptyIndex = parseInt(btn.dataset.ptyIndex, 10);
    if (!this.#currentAgentId || !this.#hubConn || ptyIndex < 1) return;

    // Disable to prevent double-clicks; re-enabled on next agentList render
    btn.disabled = true;
    btn.dataset.pending = "";

    this.#hubConn.removeSession(this.#currentAgentId, ptyIndex);
  }

  async #initConnection() {
    if (!this.hubIdValue) return;

    this.#hubConn = await ConnectionManager.acquire(
      HubConnection,
      this.hubIdValue,
      { hubId: this.hubIdValue },
    );

    // Guard: if disconnected during async acquire, release and bail
    if (this.#disconnected) {
      this.#hubConn.release();
      this.#hubConn = null;
      return;
    }

    // Listen for agent list updates
    this.#unsubscribers.push(
      this.#hubConn.on("agentList", (agents) => {
        this.#handleAgentList(agents);
      }),
    );

    // Also update on reconnection (agent data may have changed)
    this.#unsubscribers.push(
      this.#hubConn.onConnected(() => {
        this.#hubConn.requestAgents();
      }),
    );
  }

  #handleAgentList(agents) {
    // Find the agent at our index
    const agent = agents[this.agentIndexValue];
    if (!agent) return;

    this.#currentAgentId = agent.id;

    let sessions;

    // Use sessions array if available, fall back to legacy
    if (agent.sessions && agent.sessions.length > 0) {
      sessions = agent.sessions;
    } else if (agent.has_server_pty) {
      // Legacy: 2 hardcoded tabs
      sessions = [
        { name: "agent" },
        { name: "server", port_forward: true },
      ];
    } else {
      sessions = [{ name: "agent" }];
    }

    // If the current PTY was removed (index out of bounds), navigate to primary
    if (this.ptyIndexValue >= sessions.length) {
      window.Turbo.visit(
        `/hubs/${this.hubIdValue}/agents/${this.agentIndexValue}/ptys/0`,
        { action: "replace" },
      );
      return;
    }

    this.#renderTabs(sessions);
    this.#updatePreview(sessions);
  }

  #renderTabs(sessions) {
    const newTabBar = this.tabBarTarget.cloneNode(false);

    sessions.forEach((session, index) => {
      const isActive = index === this.ptyIndexValue;
      const name = session.name;
      const label = name.charAt(0).toUpperCase() + name.slice(1);
      const closable = index > 0;

      const wrapper = document.createElement("span");
      wrapper.id = `pty-tab-${name}`;
      wrapper.className = "inline-flex items-center";

      const link = document.createElement("a");
      link.id = `pty-tab-${name}-link`;
      link.href = `/hubs/${this.hubIdValue}/agents/${this.agentIndexValue}/ptys/${index}`;
      link.dataset.turboAction = "replace";
      link.className = `px-2 py-1 text-xs font-medium transition-colors ${
        closable ? "rounded-l" : "rounded"
      } ${
        isActive
          ? "bg-zinc-700 text-zinc-100"
          : "text-zinc-500 hover:text-zinc-300"
      }`;
      link.textContent = label;
      wrapper.appendChild(link);

      if (closable) {
        const close = document.createElement("button");
        close.id = `pty-tab-${name}-close`;
        close.type = "button";
        close.dataset.ptyIndex = index;
        close.dataset.action = "pty-tabs#removeSession";
        close.className = `px-1 py-1 text-xs rounded-r transition-colors ${
          isActive
            ? "bg-zinc-700 text-zinc-400 hover:text-zinc-100"
            : "text-zinc-600 hover:text-zinc-300"
        }`;
        close.setAttribute("aria-label", `Close ${label}`);
        close.textContent = "\u00d7";
        wrapper.appendChild(close);
      }

      newTabBar.appendChild(wrapper);
    });

    window.Turbo.morphElements(this.tabBarTarget, newTabBar, {
      morphStyle: "innerHTML",
    });
  }

  #updatePreview(sessions) {
    if (!this.hasPreviewTarget) return;

    const current = sessions[this.ptyIndexValue];
    if (current?.port_forward) {
      // Update href to point to the correct PTY index
      const link = this.previewTarget.querySelector("a");
      if (link) {
        link.href = `/hubs/${this.hubIdValue}/agents/${this.agentIndexValue}/${this.ptyIndexValue}/preview`;
      }
      this.previewTarget.hidden = false;
    } else {
      this.previewTarget.hidden = true;
    }
  }
}
