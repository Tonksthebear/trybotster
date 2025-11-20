# frozen_string_literal: true

require "test_helper"

module Github
  class WebhooksControllerTest < ActionDispatch::IntegrationTest
    setup do
      @webhook_secret = ENV.fetch("GITHUB_WEBHOOK_SECRET", "test_secret")
      @user = users(:one) # Assumes you have a fixture
    end

    def sign_webhook_payload(payload)
      body = payload.to_json
      signature = "sha256=" + OpenSSL::HMAC.hexdigest(
        OpenSSL::Digest.new("sha256"),
        @webhook_secret,
        body
      )
      [ body, signature ]
    end

    test "extract_linked_issues finds Fixes references" do
      controller = Github::WebhooksController.new

      pr_body = "This PR fixes #123 and resolves #456"
      issues = controller.send(:extract_linked_issues, pr_body)

      assert_equal [ 123, 456 ], issues
    end

    test "extract_linked_issues finds Closes references" do
      controller = Github::WebhooksController.new

      pr_body = "Closes #789"
      issues = controller.send(:extract_linked_issues, pr_body)

      assert_equal [ 789 ], issues
    end

    test "extract_linked_issues handles case insensitive" do
      controller = Github::WebhooksController.new

      pr_body = "FIXES #100, closes #200, ResolveS #300"
      issues = controller.send(:extract_linked_issues, pr_body)

      assert_equal [ 100, 200, 300 ], issues
    end

    test "extract_linked_issues returns empty for no matches" do
      controller = Github::WebhooksController.new

      pr_body = "This is just a regular PR description"
      issues = controller.send(:extract_linked_issues, pr_body)

      assert_equal [], issues
    end

    test "extract_linked_issues removes duplicates" do
      controller = Github::WebhooksController.new

      pr_body = "Fixes #123 and also fixes #123 again"
      issues = controller.send(:extract_linked_issues, pr_body)

      assert_equal [ 123 ], issues
    end

    test "issue_comment webhook with @trybotster mention creates bot message" do
      payload = {
        action: "created",
        issue: {
          number: 42,
          title: "Test Issue",
          body: "Issue body",
          html_url: "https://github.com/test/repo/issues/42",
          pull_request: nil
        },
        comment: {
          id: 999,
          body: "@trybotster please help",
          user: { login: "testuser" }
        },
        repository: {
          full_name: "test/repo"
        }
      }

      body, signature = sign_webhook_payload(payload)

      assert_difference "Bot::Message.count", 1 do
        post "/webhooks/github",
          params: body,
          headers: {
            "Content-Type" => "application/json",
            "X-GitHub-Event" => "issue_comment",
            "X-Hub-Signature-256" => signature
          }
      end

      assert_response :success

      message = Bot::Message.last
      assert_equal "github_mention", message.event_type
      assert_equal "test/repo", message.payload["repo"]
      assert_equal 42, message.payload["issue_number"]
      assert_equal false, message.payload["is_pr"]
    end

    test "issue_comment on PR without linked issue creates PR bot message" do
      payload = {
        action: "created",
        issue: {
          number: 100,
          title: "Test PR",
          body: "PR body without issue link",
          html_url: "https://github.com/test/repo/pull/100",
          pull_request: { url: "https://api.github.com/repos/test/repo/pulls/100" }
        },
        comment: {
          id: 888,
          body: "@trybotster review this",
          user: { login: "reviewer" }
        },
        repository: {
          full_name: "test/repo"
        }
      }

      body, signature = sign_webhook_payload(payload)

      # Mock the fetch_linked_issue_for_pr to return nil (no linked issue)
      Github::WebhooksController.any_instance.stubs(:fetch_linked_issue_for_pr).returns(nil)

      assert_difference "Bot::Message.count", 1 do
        post "/webhooks/github",
          params: body,
          headers: {
            "Content-Type" => "application/json",
            "X-GitHub-Event" => "issue_comment",
            "X-Hub-Signature-256" => signature
          }
      end

      message = Bot::Message.last
      assert_equal 100, message.payload["issue_number"]
      assert_equal true, message.payload["is_pr"]
    end

    test "issue_comment on PR with linked issue routes to issue" do
      payload = {
        action: "created",
        issue: {
          number: 200,
          title: "Test PR with link",
          body: "Fixes #50",
          html_url: "https://github.com/test/repo/pull/200",
          pull_request: { url: "https://api.github.com/repos/test/repo/pulls/200" }
        },
        comment: {
          id: 777,
          body: "@trybotster update needed",
          user: { login: "commenter" }
        },
        repository: {
          full_name: "test/repo"
        }
      }

      body, signature = sign_webhook_payload(payload)

      # Mock the fetch to return linked issue #50
      Github::WebhooksController.any_instance.stubs(:fetch_linked_issue_for_pr).returns(50)

      assert_difference "Bot::Message.count", 1 do
        post "/webhooks/github",
          params: body,
          headers: {
            "Content-Type" => "application/json",
            "X-GitHub-Event" => "issue_comment",
            "X-Hub-Signature-256" => signature
          }
      end

      message = Bot::Message.last
      assert_equal 50, message.payload["issue_number"], "Should route to linked issue #50"
      assert_equal false, message.payload["is_pr"], "Should be marked as issue, not PR"
    end

    test "ignores comments from bot users" do
      payload = {
        action: "created",
        issue: {
          number: 42,
          title: "Test Issue",
          body: "Issue body",
          html_url: "https://github.com/test/repo/issues/42"
        },
        comment: {
          id: 999,
          body: "@trybotster please help",
          user: { login: "trybotster" }
        },
        repository: {
          full_name: "test/repo"
        }
      }

      body, signature = sign_webhook_payload(payload)

      assert_no_difference "Bot::Message.count" do
        post "/webhooks/github",
          params: body,
          headers: {
            "Content-Type" => "application/json",
            "X-GitHub-Event" => "issue_comment",
            "X-Hub-Signature-256" => signature
          }
      end
    end
  end
end
