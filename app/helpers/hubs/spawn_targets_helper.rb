# frozen_string_literal: true

module Hubs
  module SpawnTargetsHelper
    def spawn_target_browser_payload(hub)
      {
        current_hub_context: {
          name: hub.name.presence || "Current hub",
          label: "Current Hub",
          summary: "Admitted spawn targets are selected explicitly in active create-session flows.",
          integration_note: "Admit a directory here to make it available to the runtime immediately."
        },
        home_path: Dir.home,
        targets: []
      }
    end
  end
end
