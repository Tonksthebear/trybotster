import { Controller } from "@hotwired/stimulus";
import { ConnectionManager, HubConnection } from "connections";

/**
 * Dynamic PTY Tab Controller
 *
 * Renders PTY session tabs based on the sessions[] array from agent info.
 * Replaces the hardcoded 2-tab (Agent/Server) switcher with N dynamic tabs.
 *
 * Connects to the HubConnection to request agent info, then builds tab
 * links from the sessions array. Falls back to a single "Agent" tab if
 * session data is not available (backward compat).
 */
export default class extends Controller {
  static targets = ["tabBar"];

  static values = {
    hubId: String,
    agentIndex: Number,
    ptyIndex: { type: Number, default: 0 },
  };

  #hubConn = null;
  #unsubscribers = [];

  connect() {
    // Render default tab immediately (before data arrives)
    this.#renderTabs([{ name: "agent" }]);
    this.#initConnection();
  }

  disconnect() {
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

    // Use sessions array if available, fall back to legacy
    if (agent.sessions && agent.sessions.length > 0) {
      this.#renderTabs(agent.sessions);
    } else if (agent.has_server_pty) {
      // Legacy: 2 hardcoded tabs
      this.#renderTabs([
        { name: "agent" },
        { name: "server", port_forward: true },
      ]);
    }
    // Otherwise keep the default single "agent" tab
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
}
