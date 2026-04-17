# frozen_string_literal: true

require "test_helper"

class HubListChannelTest < ActionCable::Channel::TestCase
  tests HubListChannel

  setup do
    @user = users(:primary_user)
    stub_connection current_user: @user
  end

  test "subscribes and streams hub list updates for the current user" do
    subscribe

    assert subscription.confirmed?
    assert_has_stream_for @user
  end
end
