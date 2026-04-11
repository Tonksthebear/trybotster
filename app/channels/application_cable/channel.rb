# frozen_string_literal: true

module ApplicationCable
  class Channel < ActionCable::Channel::Base
    before_subscribe :reject_unless_authorized

    private

    # Default-deny: anonymous preview connections are rejected unless the
    # channel explicitly opts in via `def self.preview_allowed? = true`.
    # Authenticated users (current_user present) always pass.
    def reject_unless_authorized
      return if current_user.present?

      reject unless self.class.respond_to?(:preview_allowed?) && self.class.preview_allowed?
    end
  end
end
