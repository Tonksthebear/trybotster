# frozen_string_literal: true

class AddPublicPreviewSessionsToHubs < ActiveRecord::Migration[8.0]
  def change
    add_column :hubs, :public_preview_sessions, :json, default: []
  end
end
