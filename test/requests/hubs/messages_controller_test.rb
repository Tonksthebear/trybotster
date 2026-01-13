# frozen_string_literal: true

require "test_helper"

# Tests for message polling endpoints.
#
# The CLI uses these endpoints to:
# 1. Poll for pending messages (GET /hubs/:hub_id/messages)
# 2. Acknowledge processed messages (PATCH /hubs/:hub_id/messages/:id)
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
  # GET /hubs/:hub_id/messages - Poll for messages
  # ==========================================================================

  test "GET /hubs/:hub_id/messages returns 401 without authentication" do
    get hub_messages_url(@hub),
      headers: json_headers

    assert_response :unauthorized
  end

  test "GET /hubs/:hub_id/messages returns 404 for unknown hub" do
    get hub_messages_url(hub_id: 999999),
      headers: auth_headers_for(:jason)

    assert_response :not_found
    assert_json_error("Hub not found")
  end

  test "GET /hubs/:hub_id/messages returns empty array when no messages" do
    get hub_messages_url(@hub),
      headers: auth_headers_for(:jason)

    assert_response :ok
    json = assert_json_keys(:messages, :count)

    assert_equal [], json["messages"]
    assert_equal 0, json["count"]
  end

  test "GET /hubs/:hub_id/messages returns pending messages for hub's repo" do
    # Create a pending message for the hub's repo
    message = Bot::Message.create!(
      event_type: "github_mention",
      payload: { repo: @hub.repo, issue_number: 42, comment_body: "Test message" },
      status: "pending"
    )

    get hub_messages_url(@hub),
      headers: auth_headers_for(:jason)

    assert_response :ok
    json = assert_json_keys(:messages, :count)

    assert_equal 1, json["count"]
    assert_equal 1, json["messages"].length

    msg = json["messages"].first
    assert_equal message.id, msg["id"]
    assert_equal "github_mention", msg["event_type"]
    assert_equal @hub.repo, msg["payload"]["repo"]
  end

  test "GET /hubs/:hub_id/messages returns correct message fields" do
    message = Bot::Message.create!(
      event_type: "github_mention",
      payload: { repo: @hub.repo, issue_number: 123 },
      status: "pending"
    )

    get hub_messages_url(@hub),
      headers: auth_headers_for(:jason)

    assert_response :ok
    json = assert_json_response

    msg = json["messages"].first
    assert msg.key?("id")
    assert msg.key?("event_type")
    assert msg.key?("payload")
    assert msg.key?("created_at")
  end

  test "GET /hubs/:hub_id/messages claims messages for user" do
    message = Bot::Message.create!(
      event_type: "github_mention",
      payload: { repo: @hub.repo },
      status: "pending"
    )

    get hub_messages_url(@hub),
      headers: auth_headers_for(:jason)

    assert_response :ok

    message.reload
    assert message.claimed?
    assert_equal users(:jason).id, message.claimed_by_user_id
    assert_equal "sent", message.status
  end

  test "GET /hubs/:hub_id/messages does not return already claimed messages" do
    # Create a message that's already claimed
    Bot::Message.create!(
      event_type: "github_mention",
      payload: { repo: @hub.repo },
      status: "sent",
      claimed_at: 1.minute.ago,
      claimed_by_user_id: users(:one).id
    )

    get hub_messages_url(@hub),
      headers: auth_headers_for(:jason)

    assert_response :ok
    json = assert_json_response

    assert_equal 0, json["count"]
  end

  test "GET /hubs/:hub_id/messages does not return messages for other repos" do
    # Create a message for a different repo
    Bot::Message.create!(
      event_type: "github_mention",
      payload: { repo: "other/repo" },
      status: "pending"
    )

    get hub_messages_url(@hub),
      headers: auth_headers_for(:jason)

    assert_response :ok
    json = assert_json_response

    assert_equal 0, json["count"]
  end

  test "GET /hubs/:hub_id/messages limits to 50 messages" do
    # Create 60 pending messages
    60.times do |i|
      Bot::Message.create!(
        event_type: "github_mention",
        payload: { repo: @hub.repo, issue_number: i },
        status: "pending"
      )
    end

    get hub_messages_url(@hub),
      headers: auth_headers_for(:jason)

    assert_response :ok
    json = assert_json_response

    assert_equal 50, json["messages"].length
    assert_equal 50, json["count"]
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
