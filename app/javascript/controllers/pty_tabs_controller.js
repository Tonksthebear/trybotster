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
  static targets = ["tabBar", "preview"];

  static values = {
    hubId: String,
    agentIndex: Number,
    ptyIndex: { type: Number, default: 0 },
  };

  #hubConn = null;
  #unsubscribers = [];
  #disconnected = false;

  connect() {
    this.#disconnected = false;
    // Render default tab immediately (before data arrives)
    this.#renderTabs([{ name: "agent" }]);
    this.#initConnection();
  }

  disconnect() {
    this.#disconnected = true;
    this.#unsubscribers.forEach((unsub) => unsub());
    this.#unsubscribers = [];
    this.#hubConn?.release();
    this.#hubConn = null;
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

    this.#renderTabs(sessions);
    this.#updatePreview(sessions);
  }

  #renderTabs(sessions) {
    const tabBar = this.tabBarTarget;
    tabBar.innerHTML = "";

    sessions.forEach((session, index) => {
      const isActive = index === this.ptyIndexValue;
      const link = document.createElement("a");
      link.href = `/hubs/${this.hubIdValue}/agents/${this.agentIndexValue}/ptys/${index}`;
      link.dataset.turboAction = "replace";
      link.className = `px-2 py-1 text-xs font-medium rounded transition-colors ${
        isActive
          ? "bg-zinc-700 text-zinc-100"
          : "text-zinc-500 hover:text-zinc-300"
      }`;

      // Capitalize session name
      const name = session.name;
      link.textContent = name.charAt(0).toUpperCase() + name.slice(1);
      tabBar.appendChild(link);
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
