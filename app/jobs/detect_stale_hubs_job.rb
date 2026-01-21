# frozen_string_literal: true

# Detects hubs that have gone stale (no heartbeat for 2+ minutes) and broadcasts
# status updates to connected browsers. This handles ungraceful CLI shutdowns
# where the DELETE /hubs/:id endpoint is never called.
class DetectStaleHubsJob < ApplicationJob
  queue_as :default

  def perform
    # Find hubs marked alive but with stale heartbeats
    stale_hubs = Hub.where(alive: true).where("last_seen_at <= ?", 2.minutes.ago)

    stale_hubs.find_each do |hub|
      Rails.logger.info "[DetectStaleHubsJob] Marking hub #{hub.id} as offline (last seen: #{hub.last_seen_at})"

      # Mark as not alive and broadcast the status change
      hub.update!(alive: false)
      hub.broadcast_update!
    end

    Rails.logger.info "[DetectStaleHubsJob] Processed #{stale_hubs.count} stale hubs" if stale_hubs.any?
  end
end
