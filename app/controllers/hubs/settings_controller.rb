# frozen_string_literal: true

module Hubs
  class SettingsController < ApplicationController
    include TemplateReadable

    before_action :authenticate_user!
    before_action :set_hub

    def show
      respond_to do |format|
        format.html { render "spa/show", layout: "spa" }
        format.json do
          templates = template_catalog
          render json: {
            configMetadata: config_metadata,
            templates: templates,
            agentTemplates: templates["agents"] || [],
            hubName: Current.hub.name,
            hubIdentifier: Current.hub.identifier
          }
        end
      end
    end

    def update
      if Current.hub.update(hub_params)
        respond_to do |format|
          format.html { redirect_to hub_settings_path(Current.hub) }
          format.json { render json: hub_json(Current.hub) }
        end
      else
        respond_to do |format|
          format.html { redirect_to hub_settings_path(Current.hub), alert: Current.hub.errors.full_messages.to_sentence }
          format.json { render json: { errors: Current.hub.errors.full_messages }, status: :unprocessable_entity }
        end
      end
    end

    def destroy
      Current.hub.destroy!
      respond_to do |format|
        format.html { redirect_to hubs_path }
        format.json { head :no_content }
      end
    end

    private

    def set_hub
      Current.hub = current_user.hubs.find_by(id: params[:hub_id])
      redirect_to hubs_path, alert: "Hub not found" unless Current.hub
    end

    def hub_params
      params.require(:hub).permit(:name)
    end

    def hub_json(hub)
      {
        id: hub.id,
        name: hub.name,
        identifier: hub.identifier
      }
    end

    def config_metadata
      {
        session_files: {
          "initialization" => { description: "Session startup script",
            default: "#!/bin/bash\n# Session initialization\n# Commands run when this session starts\n" }
        }
      }
    end

    # Parse app/templates/**/*.{lua,sh} into a grouped catalog.
    # Each template has @tag metadata in comment headers.
    # Lua uses `-- @tag`, shell uses `# @tag`.
    def template_catalog
      Dir.glob(Rails.root.join("app/templates/**/*.{lua,sh}")).filter_map { |path|
        content = File.read(path)
        meta = extract_template_metadata(content)
        next unless meta[:template] && meta[:category] && meta[:dest]

        ext = File.extname(path)
        basename = File.basename(path, ext)

        {
          slug: "#{meta[:category]}-#{basename}",
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
  end
end
