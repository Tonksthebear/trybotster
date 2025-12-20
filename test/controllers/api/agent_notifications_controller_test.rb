# frozen_string_literal: true

require "test_helper"
require "ostruct"

module Api
  class AgentNotificationsControllerTest < ActionDispatch::IntegrationTest
    setup do
      # Create fresh users to avoid fixture encryption issues
      @user = User.create!(
        email: "agent_test_user@example.com",
        username: "agent_test_user",
        github_app_token: "test_token_for_user",
        github_app_token_expires_at: 1.hour.from_now
      )
      @user.generate_api_key
      @user.save!

      @user_without_github = User.create!(
        email: "agent_test_user2@example.com",
        username: "agent_test_user2"
      )
      @user_without_github.generate_api_key
      @user_without_github.save!
    end

    teardown do
      @user&.destroy
      @user_without_github&.destroy
    end

    test "create requires API key" do
      post api_agent_notifications_url,
           params: { repo: "owner/repo", issue_number: 1, notification_type: "bell" },
           as: :json

      assert_response :unauthorized
      json = JSON.parse(response.body)
      assert_equal "API key required", json["error"]
    end

    test "create with invalid API key returns unauthorized" do
      post api_agent_notifications_url,
           params: { repo: "owner/repo", issue_number: 1, notification_type: "bell" },
           headers: { "X-API-Key" => "invalid_key" },
           as: :json

      assert_response :unauthorized
      json = JSON.parse(response.body)
      assert_equal "Invalid API key", json["error"]
    end

    test "create requires repo parameter" do
      post api_agent_notifications_url,
           params: { issue_number: 1, notification_type: "bell" },
           headers: { "X-API-Key" => @user.api_key },
           as: :json

      assert_response :unprocessable_entity
      json = JSON.parse(response.body)
      assert_equal "repo and issue_number required", json["error"]
    end

    test "create requires issue_number parameter" do
      post api_agent_notifications_url,
           params: { repo: "owner/repo", notification_type: "bell" },
           headers: { "X-API-Key" => @user.api_key },
           as: :json

      assert_response :unprocessable_entity
      json = JSON.parse(response.body)
      assert_equal "repo and issue_number required", json["error"]
    end

    test "create requires GitHub authorization" do
      post api_agent_notifications_url,
           params: { repo: "owner/repo", issue_number: 1, notification_type: "bell" },
           headers: { "X-API-Key" => @user_without_github.api_key },
           as: :json

      assert_response :unauthorized
      json = JSON.parse(response.body)
      assert_equal "GitHub App not authorized", json["error"]
    end

    test "create posts GitHub comment on bell notification" do
      mock_comment = OpenStruct.new(id: 1, html_url: "https://github.com/owner/repo/issues/1#issuecomment-1")

      # Store original methods
      original_get_installation = Github::App.method(:get_installation_for_repo)
      original_installation_client = Github::App.method(:installation_client)

      begin
        # Override class methods
        Github::App.define_singleton_method(:get_installation_for_repo) do |_token, _repo|
          { success: true, installation_id: 12345, account: "owner" }
        end

        mock_client = Object.new
        mock_client.define_singleton_method(:add_comment) { |_repo, _issue, _body| mock_comment }

        Github::App.define_singleton_method(:installation_client) { |_id| mock_client }

        post api_agent_notifications_url,
             params: { repo: "owner/repo", issue_number: 1, notification_type: "bell" },
             headers: { "X-API-Key" => @user.api_key },
             as: :json

        assert_response :created
        json = JSON.parse(response.body)
        assert json["success"]
        assert_equal "https://github.com/owner/repo/issues/1#issuecomment-1", json["comment_url"]
      ensure
        # Restore original methods
        Github::App.define_singleton_method(:get_installation_for_repo, original_get_installation)
        Github::App.define_singleton_method(:installation_client, original_installation_client)
      end
    end

    test "create posts GitHub comment with osc9 message" do
      mock_comment = OpenStruct.new(id: 2, html_url: "https://github.com/owner/repo/issues/2#issuecomment-2")
      captured_body = nil

      original_get_installation = Github::App.method(:get_installation_for_repo)
      original_installation_client = Github::App.method(:installation_client)

      begin
        Github::App.define_singleton_method(:get_installation_for_repo) do |_token, _repo|
          { success: true, installation_id: 12345, account: "owner" }
        end

        mock_client = Object.new
        mock_client.define_singleton_method(:add_comment) do |_repo, _issue, body|
          captured_body = body
          mock_comment
        end

        Github::App.define_singleton_method(:installation_client) { |_id| mock_client }

        post api_agent_notifications_url,
             params: { repo: "owner/repo", issue_number: 2, notification_type: "osc9:Task completed" },
             headers: { "X-API-Key" => @user.api_key },
             as: :json

        assert_response :created
        assert_includes captured_body, "Task completed"
      ensure
        Github::App.define_singleton_method(:get_installation_for_repo, original_get_installation)
        Github::App.define_singleton_method(:installation_client, original_installation_client)
      end
    end

    test "create posts GitHub comment with osc777 title and body" do
      mock_comment = OpenStruct.new(id: 3, html_url: "https://github.com/owner/repo/issues/3#issuecomment-3")
      captured_body = nil

      original_get_installation = Github::App.method(:get_installation_for_repo)
      original_installation_client = Github::App.method(:installation_client)

      begin
        Github::App.define_singleton_method(:get_installation_for_repo) do |_token, _repo|
          { success: true, installation_id: 12345, account: "owner" }
        end

        mock_client = Object.new
        mock_client.define_singleton_method(:add_comment) do |_repo, _issue, body|
          captured_body = body
          mock_comment
        end

        Github::App.define_singleton_method(:installation_client) { |_id| mock_client }

        post api_agent_notifications_url,
             params: { repo: "owner/repo", issue_number: 3, notification_type: "osc777:Build Status:Tests passed" },
             headers: { "X-API-Key" => @user.api_key },
             as: :json

        assert_response :created
        assert_includes captured_body, "Build Status"
        assert_includes captured_body, "Tests passed"
      ensure
        Github::App.define_singleton_method(:get_installation_for_repo, original_get_installation)
        Github::App.define_singleton_method(:installation_client, original_installation_client)
      end
    end

    test "create returns error when installation not found" do
      original_get_installation = Github::App.method(:get_installation_for_repo)

      begin
        Github::App.define_singleton_method(:get_installation_for_repo) do |_token, _repo|
          { success: false, error: "No installation found for owner" }
        end

        post api_agent_notifications_url,
             params: { repo: "owner/repo", issue_number: 1, notification_type: "bell" },
             headers: { "X-API-Key" => @user.api_key },
             as: :json

        assert_response :unprocessable_entity
        json = JSON.parse(response.body)
        assert_equal "No installation found for owner", json["error"]
      ensure
        Github::App.define_singleton_method(:get_installation_for_repo, original_get_installation)
      end
    end

    test "create returns error when GitHub comment fails" do
      original_get_installation = Github::App.method(:get_installation_for_repo)
      original_installation_client = Github::App.method(:installation_client)

      begin
        Github::App.define_singleton_method(:get_installation_for_repo) do |_token, _repo|
          { success: true, installation_id: 12345, account: "owner" }
        end

        mock_client = Object.new
        mock_client.define_singleton_method(:add_comment) do |_repo, _issue, _body|
          raise StandardError, "Issue not found"
        end

        Github::App.define_singleton_method(:installation_client) { |_id| mock_client }

        post api_agent_notifications_url,
             params: { repo: "owner/repo", issue_number: 999, notification_type: "bell" },
             headers: { "X-API-Key" => @user.api_key },
             as: :json

        assert_response :unprocessable_entity
        json = JSON.parse(response.body)
        assert_includes json["error"], "Issue not found"
      ensure
        Github::App.define_singleton_method(:get_installation_for_repo, original_get_installation)
        Github::App.define_singleton_method(:installation_client, original_installation_client)
      end
    end

    test "create posts GitHub comment on question_asked notification" do
      mock_comment = OpenStruct.new(id: 4, html_url: "https://github.com/owner/repo/issues/4#issuecomment-4")
      captured_body = nil

      original_get_installation = Github::App.method(:get_installation_for_repo)
      original_installation_client = Github::App.method(:installation_client)

      begin
        Github::App.define_singleton_method(:get_installation_for_repo) do |_token, _repo|
          { success: true, installation_id: 12345, account: "owner" }
        end

        mock_client = Object.new
        mock_client.define_singleton_method(:add_comment) do |_repo, _issue, body|
          captured_body = body
          mock_comment
        end

        Github::App.define_singleton_method(:installation_client) { |_id| mock_client }

        post api_agent_notifications_url,
             params: { repo: "owner/repo", issue_number: 4, notification_type: "question_asked" },
             headers: { "X-API-Key" => @user.api_key },
             as: :json

        assert_response :created
        assert_includes captured_body, "Agent is asking a question"
        assert_includes captured_body, "waiting for your input"
      ensure
        Github::App.define_singleton_method(:get_installation_for_repo, original_get_installation)
        Github::App.define_singleton_method(:installation_client, original_installation_client)
      end
    end
  end
end
