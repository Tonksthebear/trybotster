# frozen_string_literal: true

require "test_helper"
require "minitest/mock"

class Hubs::NotificationsControllerTest < ActionDispatch::IntegrationTest
  setup do
    @user = users(:jason)
    @hub = hubs(:active_hub)

    # Create a fresh device token so deterministic encryption works for lookup
    @device = @user.devices.create!(
      name: "Notif Test CLI",
      device_type: "cli",
      fingerprint: SecureRandom.hex(8).scan(/../).join(":")
    )
    @device_token = @device.create_device_token!(name: "Notif Test Token")
    @auth_headers = { "Authorization" => "Bearer #{@device_token.token}" }
  end

  teardown do
    @device_token&.destroy
    @device&.destroy
  end

  # --- OSC9 notification format ---

  test "creates notification with osc9 format and posts GitHub comment" do
    with_github_stubs do
      post hub_notifications_path(@hub),
        headers: @auth_headers,
        params: { notification_type: "osc9:Build completed successfully", repo: "owner/repo", issue_number: 42 },
        as: :json

      assert_response :created
      json = JSON.parse(response.body)
      assert json["success"]
      assert_equal "https://github.com/owner/repo/issues/42#issuecomment-1", json["comment_url"]
    end
  end

  test "creates notification with osc9 format without message body" do
    with_github_stubs do
      post hub_notifications_path(@hub),
        headers: @auth_headers,
        params: { notification_type: "osc9:", repo: "owner/repo", issue_number: 42 },
        as: :json

      assert_response :created
    end
  end

  # --- OSC777 notification format ---

  test "creates notification with osc777 format including title and body" do
    with_github_stubs do
      post hub_notifications_path(@hub),
        headers: @auth_headers,
        params: { notification_type: "osc777:Tests Passed:All 42 tests green", repo: "owner/repo", issue_number: 7 },
        as: :json

      assert_response :created
      json = JSON.parse(response.body)
      assert json["success"]
    end
  end

  test "creates notification with osc777 format with title only" do
    with_github_stubs do
      post hub_notifications_path(@hub),
        headers: @auth_headers,
        params: { notification_type: "osc777:Done", repo: "owner/repo", issue_number: 7 },
        as: :json

      assert_response :created
    end
  end

  # --- Bell notification type ---

  test "creates notification with bell type" do
    with_github_stubs do
      post hub_notifications_path(@hub),
        headers: @auth_headers,
        params: { notification_type: "bell", repo: "owner/repo", issue_number: 1 },
        as: :json

      assert_response :created
    end
  end

  # --- Question asked notification type ---

  test "creates notification with question_asked type" do
    with_github_stubs do
      post hub_notifications_path(@hub),
        headers: @auth_headers,
        params: { notification_type: "question_asked", repo: "owner/repo", issue_number: 1 },
        as: :json

      assert_response :created
    end
  end

  # --- invocation_url parsing ---

  test "creates notification using invocation_url instead of repo and issue_number" do
    with_github_stubs do
      post hub_notifications_path(@hub),
        headers: @auth_headers,
        params: { notification_type: "bell", invocation_url: "https://github.com/owner/repo/issues/99" },
        as: :json

      assert_response :created
    end
  end

  test "creates notification using pull request invocation_url" do
    with_github_stubs do
      post hub_notifications_path(@hub),
        headers: @auth_headers,
        params: { notification_type: "bell", invocation_url: "https://github.com/owner/repo/pull/55" },
        as: :json

      assert_response :created
    end
  end

  test "returns error for invalid invocation_url format" do
    with_github_stubs do
      post hub_notifications_path(@hub),
        headers: @auth_headers,
        params: { notification_type: "bell", invocation_url: "https://not-github.com/something" },
        as: :json

      assert_response :unprocessable_entity
      json = JSON.parse(response.body)
      assert_equal "Invalid invocation_url format", json["error"]
    end
  end

  # --- Authentication required ---

  test "returns unauthorized without any credentials" do
    post hub_notifications_path(@hub),
      params: { notification_type: "bell", repo: "owner/repo", issue_number: 1 },
      as: :json

    assert_response :unauthorized
    json = JSON.parse(response.body)
    assert_equal "API key required", json["error"]
  end

  test "returns unauthorized with invalid API key" do
    post hub_notifications_path(@hub),
      headers: { "Authorization" => "Bearer btstr_invalid_garbage" },
      params: { notification_type: "bell", repo: "owner/repo", issue_number: 1 },
      as: :json

    assert_response :unauthorized
    json = JSON.parse(response.body)
    assert_equal "Invalid API key", json["error"]
  end

  # --- Missing/invalid parameters ---

  test "returns error when repo is missing" do
    with_github_stubs do
      post hub_notifications_path(@hub),
        headers: @auth_headers,
        params: { notification_type: "bell", issue_number: 1 },
        as: :json

      assert_response :unprocessable_entity
      json = JSON.parse(response.body)
      assert_match(/repo and issue_number required/, json["error"])
    end
  end

  test "returns error when issue_number is missing" do
    with_github_stubs do
      post hub_notifications_path(@hub),
        headers: @auth_headers,
        params: { notification_type: "bell", repo: "owner/repo" },
        as: :json

      assert_response :unprocessable_entity
      json = JSON.parse(response.body)
      assert_match(/repo and issue_number required/, json["error"])
    end
  end

  test "returns error when issue_number is zero" do
    with_github_stubs do
      post hub_notifications_path(@hub),
        headers: @auth_headers,
        params: { notification_type: "bell", repo: "owner/repo", issue_number: 0 },
        as: :json

      assert_response :unprocessable_entity
    end
  end

  # --- Hub not found / authorization boundary ---

  test "returns not found for hub owned by another user" do
    other_user = users(:one)
    other_hub = Hub.create!(user: other_user, identifier: "other-notif-hub", last_seen_at: Time.current)

    with_github_stubs do
      post hub_notifications_path(other_hub),
        headers: @auth_headers,
        params: { notification_type: "bell", repo: "owner/repo", issue_number: 1 },
        as: :json

      assert_response :not_found
      json = JSON.parse(response.body)
      assert_equal "Hub not found", json["error"]
    end
  ensure
    other_hub&.destroy
  end

  test "returns not found for nonexistent hub" do
    with_github_stubs do
      post hub_notifications_path(hub_id: 999_999),
        headers: @auth_headers,
        params: { notification_type: "bell", repo: "owner/repo", issue_number: 1 },
        as: :json

      assert_response :not_found
    end
  end

  # --- GitHub App not authorized ---

  test "returns unauthorized when user has no GitHub App token" do
    @user.update!(github_app_token: nil)

    with_github_stubs do
      post hub_notifications_path(@hub),
        headers: @auth_headers,
        params: { notification_type: "bell", repo: "owner/repo", issue_number: 1 },
        as: :json

      assert_response :unauthorized
      json = JSON.parse(response.body)
      assert_equal "GitHub App not authorized", json["error"]
    end
  ensure
    @user.update!(github_app_token: "jason_token_123")
  end

  # --- GitHub installation lookup failure ---

  test "returns error when GitHub installation not found for repo" do
    installation_error = { success: false, error: "No installation found" }

    Github::App.stub(:get_installation_for_repo, installation_error) do
      Github::App.stub(:installation_client, build_mock_client) do
        post hub_notifications_path(@hub),
          headers: @auth_headers,
          params: { notification_type: "bell", repo: "owner/repo", issue_number: 1 },
          as: :json

        assert_response :unprocessable_entity
        json = JSON.parse(response.body)
        assert_equal "No installation found", json["error"]
      end
    end
  end

  # --- GitHub comment post failure ---

  test "returns error when GitHub comment posting fails" do
    installation_success = { success: true, installation_id: "inst_123" }
    failing_client = Object.new
    failing_client.define_singleton_method(:add_comment) do |_repo, _number, _body|
      raise Octokit::Error.new(body: "Not Found")
    end

    Github::App.stub(:get_installation_for_repo, installation_success) do
      Github::App.stub(:installation_client, failing_client) do
        post hub_notifications_path(@hub),
          headers: @auth_headers,
          params: { notification_type: "bell", repo: "owner/repo", issue_number: 1 },
          as: :json

        assert_response :unprocessable_entity
        json = JSON.parse(response.body)
        assert json["error"].present?
      end
    end
  end

  private

  # Wraps the block with happy-path stubs for Github::App class methods.
  def with_github_stubs(&block)
    installation_success = { success: true, installation_id: "inst_123" }

    Github::App.stub(:get_installation_for_repo, installation_success) do
      Github::App.stub(:installation_client, build_mock_client) do
        block.call
      end
    end
  end

  def build_mock_client
    mock_comment = OpenStruct.new(to_h: { html_url: "https://github.com/owner/repo/issues/42#issuecomment-1" })
    client = Object.new
    client.define_singleton_method(:add_comment) { |_repo, _number, _body| mock_comment }
    client
  end
end
