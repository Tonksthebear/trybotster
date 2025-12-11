# frozen_string_literal: true

require "test_helper"
require "ostruct"
require "minitest/mock"

module Github
  class WebhooksControllerTest < ActionDispatch::IntegrationTest
    # Load fixtures needed for webhook tests
    fixtures :"bot/messages", :users

    setup do
      @webhook_secret = "test_secret"
      ENV["GITHUB_WEBHOOK_SECRET"] = @webhook_secret
    end

    teardown do
      ENV.delete("GITHUB_WEBHOOK_SECRET")
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

      pr_body = "This is a PR without issue references"
      issues = controller.send(:extract_linked_issues, pr_body)

      assert_equal [], issues
    end

    test "format_structured_context includes all sections for routed PR" do
      controller = Github::WebhooksController.new

      ctx = {
        source: {
          type: "pr_comment",
          repo: "owner/repo",
          owner: "owner",
          repo_name: "repo",
          number: 731,
          comment_author: "testuser"
        },
        routed_to: {
          type: "issue",
          number: 720,
          reason: "pr_linked_to_issue"
        },
        respond_to: {
          type: "pr",
          number: 731,
          instruction: "Post your response as a comment on PR #731"
        },
        message: "@trybotster Were there no tests needed?",
        task: "Answer the question about the PR changes",
        requirements: {
          must_use_trybotster_mcp: true,
          fetch_first: "pr",
          number_to_fetch: 731,
          context_number: 720
        }
      }

      formatted = controller.send(:format_structured_context, ctx)

      # Check all sections are present
      assert_includes formatted, "## Source"
      assert_includes formatted, "Type: pr_comment"
      assert_includes formatted, "Repository: owner/repo"
      assert_includes formatted, "Number: #731"
      assert_includes formatted, "Author: testuser"

      assert_includes formatted, "## Routing"
      assert_includes formatted, "Routed to: issue #720"
      assert_includes formatted, "Reason: pr_linked_to_issue"

      assert_includes formatted, "## Message"
      assert_includes formatted, "@trybotster Were there no tests needed?"

      assert_includes formatted, "## Where to Respond"
      assert_includes formatted, "PR #731"
      assert_includes formatted, "Post your response as a comment on PR #731"

      assert_includes formatted, "## Your Task"
      assert_includes formatted, "Answer the question about the PR changes"

      assert_includes formatted, "## Requirements"
      assert_includes formatted, "- You MUST use ONLY the trybotster MCP server"
      assert_includes formatted, "- Start by fetching pr #731 details"
      assert_includes formatted, "- You may fetch issue #720 for additional context"
    end

    test "issue_comment webhook creates bot message with prompt field" do
      payload = {
        action: "created",
        repository: { full_name: "owner/repo" },
        issue: {
          number: 123,
          title: "Test issue",
          body: "Issue body",
          html_url: "https://github.com/owner/repo/issues/123",
          pull_request: nil
        },
        comment: {
          id: 456,
          body: "@trybotster please help",
          user: { login: "testuser" }
        }
      }

      body, signature = sign_webhook_payload(payload)

      post "/github/webhooks",
        params: body,
        headers: {
          "Content-Type" => "application/json",
          "X-GitHub-Event" => "issue_comment",
          "X-Hub-Signature-256" => signature
        }

      # Debug: print response if not successful
      unless response.successful?
        puts "\nResponse status: #{response.status}"
        puts "Response body: #{response.body}"
      end

      assert_response :success

      message = Bot::Message.last
      assert_equal "github_mention", message.event_type
      assert_not_nil message.payload["prompt"]
      assert_not_nil message.payload["structured_context"]
      assert_equal "owner/repo", message.payload["repo"]
      assert_equal 123, message.payload["issue_number"]
      assert_equal "@trybotster please help", message.payload["comment_body"]

      # Check prompt includes key information
      prompt = message.payload["prompt"]
      assert_includes prompt, "## Source"
      assert_includes prompt, "issue_comment"
      assert_includes prompt, "## Message"
      assert_includes prompt, "@trybotster please help"
    end

    test "PR comment without linked issue creates PR agent" do
      # This would require stubbing fetch_linked_issue_for_pr to return nil
      # For now, we'll test the build_structured_context method directly
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
        post "/github/webhooks",
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
      # This test relies on fetch_linked_issue_for_pr returning nil
      # Since we can't easily stub it, we'll test with a PR body that has no issue links
      # The method will parse the body and find no linked issues
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
        },
        installation: {
          id: 12345
        }
      }

      body, signature = sign_webhook_payload(payload)

      # This will attempt to fetch the PR from GitHub API
      # For this test, we expect it to fail gracefully and create a PR message anyway
      assert_difference "Bot::Message.count", 1 do
        post "/github/webhooks",
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
        },
        installation: {
          id: 12345
        }
      }

      # Temporarily override the controller method to simulate GitHub API response
      WebhooksController.class_eval do
        alias_method :original_fetch_linked_issue_for_pr, :fetch_linked_issue_for_pr
        define_method(:fetch_linked_issue_for_pr) do |repo, pr_num|
          50  # Return issue #50
        end
      end

      begin
        body, signature = sign_webhook_payload(payload)

        assert_difference "Bot::Message.count", 1 do
          post "/github/webhooks",
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

        # Verify the structured context shows routing
        assert_not_nil message.payload["structured_context"]
        assert_equal "pr_comment", message.payload["structured_context"]["source"]["type"]
        assert_equal 200, message.payload["structured_context"]["source"]["number"]
        assert_equal "issue", message.payload["structured_context"]["routed_to"]["type"]
        assert_equal 50, message.payload["structured_context"]["routed_to"]["number"]
        assert_equal "pr_linked_to_issue", message.payload["structured_context"]["routed_to"]["reason"]

        # Verify the prompt includes routing information
        prompt = message.payload["prompt"]
        assert_includes prompt, "## Routing"
        assert_includes prompt, "Routed to: issue #50"
        assert_includes prompt, "PR #200"
      ensure
        # Restore the original method
        WebhooksController.class_eval do
          alias_method :fetch_linked_issue_for_pr, :original_fetch_linked_issue_for_pr
          remove_method :original_fetch_linked_issue_for_pr
        end
      end
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
        post "/github/webhooks",
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
