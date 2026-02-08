# frozen_string_literal: true

module Hubs
  class SettingsController < ApplicationController
    before_action :authenticate_user!
    before_action :set_hub

    def show
      @config_files = config_file_definitions
    end

    private

    def set_hub
      Current.hub = current_user.hubs.find_by(id: params[:hub_id])
      redirect_to hubs_path, alert: "Hub not found" unless Current.hub
    end

    def config_file_definitions
      [
        { name: ".botster_init", description: "Agent initialization commands",
          default: "# Agent initialization\n# Commands run when a new agent PTY starts\n# Available env: $BOTSTER_REPO, $BOTSTER_ISSUE_NUMBER, $BOTSTER_BRANCH_NAME\n" },
        { name: ".botster_server", description: "Server session startup",
          default: "# Server session startup\n# Starts a dev server on $BOTSTER_TUNNEL_PORT\n# Example: npm run dev -- --port $BOTSTER_TUNNEL_PORT\n" },
        { name: ".botster_copy", description: "Files to copy into worktrees",
          default: "# Glob patterns for files to copy into worktrees\n# One pattern per line\n# Example:\n# .env\n# config/secrets.yml\n" },
        { name: ".botster_teardown", description: "Cleanup before worktree deletion",
          default: "# Teardown commands\n# Run before worktree is deleted\n" },
      ]
    end
  end
end
