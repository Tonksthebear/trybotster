# frozen_string_literal: true

module Hubs
  class SettingsController < ApplicationController
    before_action :authenticate_user!
    before_action :set_hub

    def show
      @config_metadata = config_metadata
    end

    private

    def set_hub
      Current.hub = current_user.hubs.find_by(id: params[:hub_id])
      redirect_to hubs_path, alert: "Hub not found" unless Current.hub
    end

    def config_metadata
      {
        shared_files: {
          "workspace_include" => { description: "Glob patterns for files to copy into worktrees",
            default: "# Glob patterns for files to copy into worktrees\n# One pattern per line\n# Example:\n# .env\n# config/secrets.yml\n" },
          "workspace_teardown" => { description: "Cleanup commands before worktree deletion",
            default: "# Teardown commands\n# Run before worktree is deleted\n" }
        },
        session_files: {
          "initialization" => { description: "Session startup script",
            default: "#!/bin/bash\n# Session initialization\n# Commands run when this session starts\n" }
        }
      }
    end
  end
end
