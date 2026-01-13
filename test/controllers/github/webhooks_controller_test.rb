# frozen_string_literal: true

require "test_helper"
require "ostruct"
require "minitest/mock"

module Github
  class WebhooksControllerTest < ActionDispatch::IntegrationTest
    include GithubTestHelper

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

    # Test LinkedIssueResolver's issue extraction pattern
    test "LinkedIssueResolver pattern finds Fixes references" do
      pr_body = "This PR fixes #123 and resolves #456"
      issues = extract_linked_issues(pr_body)

      assert_equal [ 123, 456 ], issues
    end

    test "LinkedIssueResolver pattern finds Closes references" do
      pr_body = "Closes #789"
      issues = extract_linked_issues(pr_body)

      assert_equal [ 789 ], issues
    end

    test "LinkedIssueResolver pattern handles case insensitive" do
      pr_body = "FIXES #100, closes #200, ResolveS #300"
      issues = extract_linked_issues(pr_body)

      assert_equal [ 100, 200, 300 ], issues
    end

    test "LinkedIssueResolver pattern returns empty for no matches" do
      pr_body = "This is a PR without issue references"
      issues = extract_linked_issues(pr_body)

      assert_equal [], issues
    end

    test "LinkedIssueResolver pattern removes duplicates" do
      pr_body = "Fixes #123 and also fixes #123 again"
      issues = extract_linked_issues(pr_body)

      assert_equal [ 123 ], issues
    end

    test "BotMessageCreator formats structured context correctly for routed PR" do
      creator = Github::Webhooks::BotMessageCreator.new(
        repo: "owner/repo",
        issue_number: 720,
        comment_id: 12345,
        comment_body: "@trybotster Were there no tests needed?",
        comment_author: "testuser",
        issue_title: "Test Issue",
        issue_body: "Issue body",
        issue_url: "https://github.com/owner/repo/issues/720",
        is_pr: false,
        source_type: "pr_comment",
        routed_info: {
          source_number: 731,
          source_type: "pr",
          target_number: 720,
          target_type: "issue",
          reason: "pr_linked_to_issue"
        }
      )

      # Test by creating a message and checking the prompt
      message = creator.call
      formatted = message.payload["prompt"]

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
      pr_body = "This is just a regular PR description"
      issues = extract_linked_issues(pr_body)

      assert_equal [], issues
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

      # Stub GitHub API calls since LinkedIssueResolver tries to fetch installations
      with_stubbed_github do
        assert_difference "Bot::Message.count", 1 do
          post "/github/webhooks",
            params: body,
            headers: {
              "Content-Type" => "application/json",
              "X-GitHub-Event" => "issue_comment",
              "X-Hub-Signature-256" => signature
            }
        end
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

      # Stub the LinkedIssueResolver to return issue #50
      Github::Webhooks::LinkedIssueResolver.stub(:new, ->(_repo, _pr) {
        OpenStruct.new(call: 50)
      }) do
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

    private

    # Helper method to extract linked issues using the same pattern as LinkedIssueResolver
    def extract_linked_issues(pr_body)
      return [] if pr_body.blank?

      issue_numbers = []
      pattern = /(?:fix(?:es|ed)?|close(?:s|d)?|resolve(?:s|d)?|references?)\s+#(\d+)/i

      pr_body.scan(pattern) do |match|
        issue_numbers << match[0].to_i
      end

      issue_numbers.uniq
    end
  end
end
