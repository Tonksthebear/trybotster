# frozen_string_literal: true

require "test_helper"

# Tests for message acknowledgment endpoint.
#
# The CLI uses this endpoint to:
# - Acknowledge processed messages (PATCH /hubs/:hub_id/messages/:id)
#
# Message delivery is now handled via WebSocket command channel,
# not HTTP polling.
#
# These tests verify the API contract the CLI expects.
#
# Note: GitHub repo access checks are bypassed in test environment
# (see User#has_github_repo_access?) to allow testing with fake repos.
#
class Hubs::MessagesControllerTest < ActionDispatch::IntegrationTest
  include ApiTestHelper

  setup do
    @hub = hubs(:active_hub)
  end

  # ==========================================================================
  # PATCH /hubs/:hub_id/messages/:id - Acknowledge message
  # ==========================================================================

  test "PATCH /hubs/:hub_id/messages/:id returns 401 without authentication" do
    message = create_claimed_message

    patch hub_message_url(@hub, message),
      headers: json_headers

    assert_response :unauthorized
  end

  test "PATCH /hubs/:hub_id/messages/:id acknowledges message" do
    message = create_claimed_message

    # The add_eyes_reaction_to_comment method is a no-op for non-github_mention events
    # or when installation_id is missing, so we don't need to stub it

    patch hub_message_url(@hub, message),
      headers: auth_headers_for(:jason)

    assert_response :ok
    json = assert_json_keys(:success, :message_id, :acknowledged_at)

    assert_equal true, json["success"]
    assert_equal message.id, json["message_id"]

    message.reload
    assert_equal "acknowledged", message.status
    assert_not_nil message.acknowledged_at
  end

  test "PATCH /hubs/:hub_id/messages/:id returns 404 for message claimed by other user" do
    # Create message claimed by different user
    message = Bot::Message.create!(
      event_type: "github_mention",
      payload: { repo: @hub.repo },
      status: "sent",
      claimed_at: Time.current,
      claimed_by_user_id: users(:one).id,
      sent_at: Time.current
    )

    patch hub_message_url(@hub, message),
      headers: auth_headers_for(:jason)

    assert_response :not_found
    assert_json_error(/not found or not claimed/i)
  end

  test "PATCH /hubs/:hub_id/messages/:id returns 404 for nonexistent message" do
    patch hub_message_url(@hub, 999999),
      headers: auth_headers_for(:jason)

    assert_response :not_found
  end

  private

  def create_claimed_message
    Bot::Message.create!(
      event_type: "github_mention",
      payload: { repo: @hub.repo },
      status: "sent",
      claimed_at: Time.current,
      claimed_by_user_id: users(:jason).id,
      sent_at: Time.current
    )
  end
end
