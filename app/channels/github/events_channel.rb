# frozen_string_literal: true

# Github::EventsChannel - Dedicated channel for GitHub event delivery to CLI.
#
# Decoupled from HubCommandChannel so GitHub acts as a plugin integration
# with its own subscription, replay, and ack path.
#
# Protocol:
# - CLI subscribes with repo (e.g., "owner/repo")
# - On subscribe: validates GitHub App access, replays pending messages
# - Real-time: new messages broadcast via github_events:{repo} stream
# - CLI acks via perform("ack", { id: N })
#
# Stream: github_events:{repo}
#
# Auth: DeviceToken Bearer (same as HubCommandChannel)
module Github
  class EventsChannel < ApplicationCable::Channel
    def subscribed
      @repo = params[:repo]
      reject and return unless @repo.present?
      reject and return unless validate_github_access!

      stream_from "github_events:#{@repo}"
      replay_pending_messages

      Rails.logger.info "[Github::EventsChannel] Subscribed: repo=#{@repo}"
    end

    def ack(data)
      msg = Integrations::Github::Message.find_by(id: data["id"])
      msg&.acknowledge! unless msg&.acknowledged?
    end

    private

    def validate_github_access!
      token = current_user.github_app_token
      return false unless token.present?

      result = ::Github::App.get_installation_for_repo(token, @repo)
      result[:success]
    rescue => e
      Rails.logger.warn "[Github::EventsChannel] Access validation failed: #{e.message}"
      false
    end

    def replay_pending_messages
      messages = Integrations::Github::Message
        .for_repo(@repo)
        .pending
        .order(created_at: :asc)
        .limit(50)

      messages.each { |msg| transmit(msg.to_wire) }

      Rails.logger.info "[Github::EventsChannel] Replayed #{messages.size} messages for #{@repo}"
    end
  end
end
