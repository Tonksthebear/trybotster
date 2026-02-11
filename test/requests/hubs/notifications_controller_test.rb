# frozen_string_literal: true

require "test_helper"

# Tests for notification endpoints.
#
# The CLI uses this endpoint to:
# - Send notifications when agents need user input (POST /hubs/:hub_id/notifications)
#
# These tests verify the API contract the CLI expects.
class Hubs::NotificationsControllerTest < ActionDispatch::IntegrationTest
  include ApiTestHelper
  include GithubTestHelper

  setup do
    @hub = hubs(:active_hub)
  end

  # ==========================================================================
  # POST /hubs/:hub_id/notifications - Send notification
  # ==========================================================================

  test "POST /hubs/:hub_id/notifications returns 401 without authentication" do
    post hub_notifications_url(@hub),
      params: { notification_type: "question_asked", repo: "owner/repo", issue_number: 42 }.to_json,
      headers: json_headers

    assert_response :unauthorized
  end

  test "POST /hubs/:hub_id/notifications returns 404 for unknown hub" do
    post hub_notifications_url(hub_id: 999999),
      params: { notification_type: "question_asked", repo: "owner/repo", issue_number: 42 }.to_json,
      headers: auth_headers_for(:jason)

    assert_response :not_found
    assert_json_error("Hub not found")
  end

  test "POST /hubs/:hub_id/notifications requires repo and issue_number" do
    post hub_notifications_url(@hub),
      params: { notification_type: "question_asked" }.to_json,
      headers: auth_headers_for(:jason)

    assert_response :unprocessable_entity
    assert_json_error(/repo and issue_number required/i)
  end

  test "POST /hubs/:hub_id/notifications accepts invocation_url" do
    with_stubbed_github do
      post hub_notifications_url(@hub),
        params: {
          notification_type: "question_asked",
          invocation_url: "https://github.com/owner/repo/issues/42"
        }.to_json,
        headers: auth_headers_for(:jason)

      assert_response :created
      json = assert_json_keys(:success, :comment_url)

      assert_equal true, json["success"]
      assert_match %r{github\.com}, json["comment_url"]
    end
  end

  test "POST /hubs/:hub_id/notifications accepts legacy repo and issue_number params" do
    with_stubbed_github do
      post hub_notifications_url(@hub),
        params: {
          notification_type: "bell",
          repo: "owner/repo",
          issue_number: 123
        }.to_json,
        headers: auth_headers_for(:jason)

      assert_response :created
      json = assert_json_keys(:success)

      assert_equal true, json["success"]
    end
  end

  test "POST /hubs/:hub_id/notifications returns error for invalid invocation_url" do
    post hub_notifications_url(@hub),
      params: {
        notification_type: "question_asked",
        invocation_url: "not-a-valid-url"
      }.to_json,
      headers: auth_headers_for(:jason)

    assert_response :unprocessable_entity
    assert_json_error(/Invalid invocation_url/i)
  end

  test "POST /hubs/:hub_id/notifications returns 401 if GitHub not authorized" do
    # User without GitHub authorization
    users(:jason).update!(github_app_token: nil)

    post hub_notifications_url(@hub),
      params: {
        notification_type: "question_asked",
        repo: "owner/repo",
        issue_number: 42
      }.to_json,
      headers: auth_headers_for(:jason)

    assert_response :unauthorized
    assert_json_error(/GitHub App not authorized/i)
  end

  test "POST /hubs/:hub_id/notifications supports different notification types" do
    with_stubbed_github do
      %w[bell question_asked osc9:custom_message osc777:Title:Body].each do |type|
        post hub_notifications_url(@hub),
          params: {
            notification_type: type,
            repo: "owner/repo",
            issue_number: 42
          }.to_json,
          headers: auth_headers_for(:jason)

        assert_response :created, "Expected success for notification_type: #{type}"
      end
    end
  end
end
