# frozen_string_literal: true

require "test_helper"
require "ostruct"
require "minitest/mock"

module Github
  class WebhooksControllerTest < ActionDispatch::IntegrationTest
    include GithubTestHelper

    fixtures :users

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

    test "MessageCreator formats structured context correctly for routed PR" do
      creator = Integrations::Github::Webhooks::MessageCreator.new(
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

      message = creator.call
      formatted = message.payload["prompt"]

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

    test "issue_comment webhook creates github message with prompt field" do
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

      assert_response :success

      message = Integrations::Github::Message.last
      assert_equal "github_mention", message.event_type
      assert_equal "owner/repo", message.repo
      assert_equal 123, message.issue_number
      assert_not_nil message.payload["prompt"]
      assert_not_nil message.payload["structured_context"]
      assert_equal "@trybotster please help", message.payload["comment_body"]

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

    test "issue_comment webhook with @trybotster mention creates github message" do
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

      assert_difference "Integrations::Github::Message.count", 1 do
        post "/github/webhooks",
          params: body,
          headers: {
            "Content-Type" => "application/json",
            "X-GitHub-Event" => "issue_comment",
            "X-Hub-Signature-256" => signature
          }
      end

      assert_response :success

      message = Integrations::Github::Message.last
      assert_equal "github_mention", message.event_type
      assert_equal "test/repo", message.repo
      assert_equal 42, message.issue_number
      assert_equal false, message.payload["is_pr"]
    end

    test "issue_comment on PR without linked issue creates PR github message" do
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

      with_stubbed_github do
        assert_difference "Integrations::Github::Message.count", 1 do
          post "/github/webhooks",
            params: body,
            headers: {
              "Content-Type" => "application/json",
              "X-GitHub-Event" => "issue_comment",
              "X-Hub-Signature-256" => signature
            }
        end
      end

      message = Integrations::Github::Message.last
      assert_equal 100, message.issue_number
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

      Integrations::Github::Webhooks::LinkedIssueResolver.stub(:new, ->(_repo, _pr) {
        OpenStruct.new(call: 50)
      }) do
        body, signature = sign_webhook_payload(payload)

        assert_difference "Integrations::Github::Message.count", 1 do
          post "/github/webhooks",
            params: body,
            headers: {
              "Content-Type" => "application/json",
              "X-GitHub-Event" => "issue_comment",
              "X-Hub-Signature-256" => signature
            }
        end

        message = Integrations::Github::Message.last
        assert_equal 50, message.issue_number, "Should route to linked issue #50"
        assert_equal false, message.payload["is_pr"], "Should be marked as issue, not PR"

        assert_not_nil message.payload["structured_context"]
        assert_equal "pr_comment", message.payload["structured_context"]["source"]["type"]
        assert_equal 200, message.payload["structured_context"]["source"]["number"]
        assert_equal "issue", message.payload["structured_context"]["routed_to"]["type"]
        assert_equal 50, message.payload["structured_context"]["routed_to"]["number"]
        assert_equal "pr_linked_to_issue", message.payload["structured_context"]["routed_to"]["reason"]

        prompt = message.payload["prompt"]
        assert_includes prompt, "## Routing"
        assert_includes prompt, "Routed to: issue #50"
        assert_includes prompt, "PR #200"
      end
    end

    test "ignores comments from trybotster itself" do
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

      assert_no_difference "Integrations::Github::Message.count" do
        post "/github/webhooks",
          params: body,
          headers: {
            "Content-Type" => "application/json",
            "X-GitHub-Event" => "issue_comment",
            "X-Hub-Signature-256" => signature
          }
      end
    end

    test "ignores comments from GitHub app bots" do
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
          user: { login: "dependabot[bot]" }
        },
        repository: {
          full_name: "test/repo"
        }
      }

      body, signature = sign_webhook_payload(payload)

      assert_no_difference "Integrations::Github::Message.count" do
        post "/github/webhooks",
          params: body,
          headers: {
            "Content-Type" => "application/json",
            "X-GitHub-Event" => "issue_comment",
            "X-Hub-Signature-256" => signature
          }
      end
    end

    test "does not ignore comments from users with bot in their name" do
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
          user: { login: "robert" }
        },
        repository: {
          full_name: "test/repo"
        }
      }

      body, signature = sign_webhook_payload(payload)

      assert_difference "Integrations::Github::Message.count", 1 do
        post "/github/webhooks",
          params: body,
          headers: {
            "Content-Type" => "application/json",
            "X-GitHub-Event" => "issue_comment",
            "X-Hub-Signature-256" => signature
          }
      end
    end

    # === Signature Validation ===

    test "rejects webhook with missing signature" do
      payload = { action: "created" }.to_json

      post "/github/webhooks",
        params: payload,
        headers: {
          "Content-Type" => "application/json",
          "X-GitHub-Event" => "issue_comment"
        }

      assert_response :unauthorized
    end

    test "rejects webhook with invalid signature" do
      payload = { action: "created" }.to_json

      post "/github/webhooks",
        params: payload,
        headers: {
          "Content-Type" => "application/json",
          "X-GitHub-Event" => "issue_comment",
          "X-Hub-Signature-256" => "sha256=invalid_signature"
        }

      assert_response :unauthorized
    end

    test "rejects webhook when no secret configured" do
      ENV.delete("GITHUB_WEBHOOK_SECRET")

      payload = { action: "created" }.to_json

      post "/github/webhooks",
        params: payload,
        headers: {
          "Content-Type" => "application/json",
          "X-GitHub-Event" => "issue_comment",
          "X-Hub-Signature-256" => "sha256=anything"
        }

      assert_response :unauthorized
    end

    test "rejects invalid JSON payload" do
      body = "not valid json"
      signature = "sha256=" + OpenSSL::HMAC.hexdigest(
        OpenSSL::Digest.new("sha256"), @webhook_secret, body
      )

      post "/github/webhooks",
        params: body,
        headers: {
          "Content-Type" => "application/json",
          "X-GitHub-Event" => "issue_comment",
          "X-Hub-Signature-256" => signature
        }

      assert_response :bad_request
    end

    # === Unknown Event Type ===

    test "returns 200 for unknown event type" do
      payload = { action: "created" }
      body, signature = sign_webhook_payload(payload)

      assert_no_difference "Integrations::Github::Message.count" do
        post "/github/webhooks",
          params: body,
          headers: {
            "Content-Type" => "application/json",
            "X-GitHub-Event" => "star",
            "X-Hub-Signature-256" => signature
          }
      end

      assert_response :success
    end

    # === Issues Event ===

    test "issues closed event creates cleanup message" do
      payload = {
        action: "closed",
        issue: {
          number: 50,
          title: "Closed Issue",
          body: "Done",
          html_url: "https://github.com/test/repo/issues/50",
          user: { login: "closer" }
        },
        repository: { full_name: "test/repo" }
      }

      body, signature = sign_webhook_payload(payload)

      assert_difference "Integrations::Github::Message.count", 1 do
        post "/github/webhooks",
          params: body,
          headers: {
            "Content-Type" => "application/json",
            "X-GitHub-Event" => "issues",
            "X-Hub-Signature-256" => signature
          }
      end

      assert_response :success
      message = Integrations::Github::Message.last
      assert_equal "agent_cleanup", message.event_type
      assert_equal 50, message.issue_number
      assert_equal "issue_closed", message.payload["reason"]
    end

    test "issues opened with @trybotster mention creates github message" do
      payload = {
        action: "opened",
        issue: {
          number: 60,
          title: "New Issue",
          body: "@trybotster please investigate this",
          html_url: "https://github.com/test/repo/issues/60",
          user: { login: "reporter" }
        },
        repository: { full_name: "test/repo" }
      }

      body, signature = sign_webhook_payload(payload)

      assert_difference "Integrations::Github::Message.count", 1 do
        post "/github/webhooks",
          params: body,
          headers: {
            "Content-Type" => "application/json",
            "X-GitHub-Event" => "issues",
            "X-Hub-Signature-256" => signature
          }
      end

      message = Integrations::Github::Message.last
      assert_equal "github_mention", message.event_type
      assert_equal 60, message.issue_number
    end

    test "issues opened without mention does not create message" do
      payload = {
        action: "opened",
        issue: {
          number: 61,
          title: "Normal Issue",
          body: "Just a regular issue",
          html_url: "https://github.com/test/repo/issues/61",
          user: { login: "reporter" }
        },
        repository: { full_name: "test/repo" }
      }

      body, signature = sign_webhook_payload(payload)

      assert_no_difference "Integrations::Github::Message.count" do
        post "/github/webhooks",
          params: body,
          headers: {
            "Content-Type" => "application/json",
            "X-GitHub-Event" => "issues",
            "X-Hub-Signature-256" => signature
          }
      end

      assert_response :success
    end

    # === Pull Request Event ===

    test "pull_request closed creates cleanup message" do
      payload = {
        action: "closed",
        pull_request: {
          number: 70,
          title: "Closed PR",
          body: "Done",
          html_url: "https://github.com/test/repo/pull/70",
          user: { login: "merger" }
        },
        repository: { full_name: "test/repo" }
      }

      body, signature = sign_webhook_payload(payload)

      assert_difference "Integrations::Github::Message.count", 1 do
        post "/github/webhooks",
          params: body,
          headers: {
            "Content-Type" => "application/json",
            "X-GitHub-Event" => "pull_request",
            "X-Hub-Signature-256" => signature
          }
      end

      message = Integrations::Github::Message.last
      assert_equal "agent_cleanup", message.event_type
      assert_equal 70, message.issue_number
      assert_equal "pr_closed", message.payload["reason"]
      assert_equal true, message.payload["is_pr"]
    end

    test "pull_request opened with @trybotster mention creates message" do
      payload = {
        action: "opened",
        pull_request: {
          number: 80,
          title: "New PR",
          body: "@trybotster review this please",
          html_url: "https://github.com/test/repo/pull/80",
          user: { login: "author" }
        },
        repository: { full_name: "test/repo" }
      }

      body, signature = sign_webhook_payload(payload)

      assert_difference "Integrations::Github::Message.count", 1 do
        post "/github/webhooks",
          params: body,
          headers: {
            "Content-Type" => "application/json",
            "X-GitHub-Event" => "pull_request",
            "X-Hub-Signature-256" => signature
          }
      end

      message = Integrations::Github::Message.last
      assert_equal "github_mention", message.event_type
      assert_equal 80, message.issue_number
      assert_equal true, message.payload["is_pr"]
    end

    # === PR Review Comment Event ===

    test "pr_review_comment with @trybotster mention creates message" do
      payload = {
        action: "created",
        pull_request: {
          number: 90,
          title: "Review PR",
          body: "Regular PR",
          html_url: "https://github.com/test/repo/pull/90",
          user: { login: "author" }
        },
        comment: {
          id: 555,
          body: "@trybotster can you fix this?",
          user: { login: "reviewer" }
        },
        repository: { full_name: "test/repo" },
        installation: { id: 12345 }
      }

      body, signature = sign_webhook_payload(payload)

      with_stubbed_github do
        assert_difference "Integrations::Github::Message.count", 1 do
          post "/github/webhooks",
            params: body,
            headers: {
              "Content-Type" => "application/json",
              "X-GitHub-Event" => "pull_request_review_comment",
              "X-Hub-Signature-256" => signature
            }
        end
      end

      message = Integrations::Github::Message.last
      assert_equal "github_mention", message.event_type
      assert_equal 90, message.issue_number
    end

    test "pr_review_comment without mention does not create message" do
      payload = {
        action: "created",
        pull_request: {
          number: 91,
          title: "Review PR",
          body: "PR body",
          html_url: "https://github.com/test/repo/pull/91",
          user: { login: "author" }
        },
        comment: {
          id: 556,
          body: "Looks good to me!",
          user: { login: "reviewer" }
        },
        repository: { full_name: "test/repo" }
      }

      body, signature = sign_webhook_payload(payload)

      assert_no_difference "Integrations::Github::Message.count" do
        post "/github/webhooks",
          params: body,
          headers: {
            "Content-Type" => "application/json",
            "X-GitHub-Event" => "pull_request_review_comment",
            "X-Hub-Signature-256" => signature
          }
      end

      assert_response :success
    end

    private

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
