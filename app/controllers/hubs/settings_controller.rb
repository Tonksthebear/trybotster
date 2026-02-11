# frozen_string_literal: true

module Hubs
  class SettingsController < ApplicationController
    before_action :authenticate_user!
    before_action :set_hub

    def show
      @config_metadata = config_metadata
      @templates = template_catalog
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

    # Parse app/templates/**/*.lua into a grouped catalog.
    # Each template has @tag metadata in comment headers.
    def template_catalog
      Dir.glob(Rails.root.join("app/templates/**/*.lua")).filter_map { |path|
        content = File.read(path)
        meta = extract_template_metadata(content)
        next unless meta[:template] && meta[:category] && meta[:dest]

        {
          slug: "#{meta[:category]}-#{File.basename(path, '.lua')}",
          name: meta[:template],
          description: meta[:description],
          category: meta[:category],
          dest: meta[:dest],
          scope: meta[:scope],
          version: meta[:version] || "1.0.0",
          content: content
        }
      }.group_by { |t| t[:category] }
    end

    def extract_template_metadata(content)
      metadata = {}
      content.each_line do |line|
        break unless line.start_with?("--")
        if (match = line.match(/^--\s*@(\w+)\s+(.+)/))
          metadata[match[1].to_sym] = match[2].strip
        end
      end
      metadata
    end
  end
end
