# frozen_string_literal: true

class RemovePublicPreviewSessionsFromHubs < ActiveRecord::Migration[8.0]
  def change
    remove_column :hubs, :public_preview_sessions, :json, default: []
  end
end
