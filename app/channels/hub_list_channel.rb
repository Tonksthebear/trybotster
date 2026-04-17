# frozen_string_literal: true

class HubListChannel < ApplicationCable::Channel
  def subscribed
    reject and return unless current_user

    stream_for current_user
  end
end
