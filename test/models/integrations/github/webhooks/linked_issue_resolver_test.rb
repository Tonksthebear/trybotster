# frozen_string_literal: true

require "test_helper"
require "minitest/mock"

class Integrations::Github::Webhooks::LinkedIssueResolverTest < ActiveSupport::TestCase
  setup do
    @repo = "owner/test-repo"
    @pr_number = 42
    @resolver = Integrations::Github::Webhooks::LinkedIssueResolver.new(@repo, @pr_number)
  end

  # === Regex extraction (unit tests for parsing logic) ===

  test "extracts issue number from 'Fixes #123'" do
    assert_equal 123, extract("Fixes #123")
  end

  test "extracts issue number from 'Closes #456'" do
    assert_equal 456, extract("Closes #456")
  end

  test "extracts issue number from 'Resolves #789'" do
    assert_equal 789, extract("Resolves #789")
  end

  test "extracts issue number from 'References #100'" do
    assert_equal 100, extract("References #100")
  end

  test "extracts issue number from 'Reference #55'" do
    assert_equal 55, extract("Reference #55")
  end

  test "handles past tense: Fixed, Closed, Resolved" do
    assert_equal 1, extract("Fixed #1")
    assert_equal 2, extract("Closed #2")
    assert_equal 3, extract("Resolved #3")
  end

  test "handles base form: Fix, Close, Resolve" do
    assert_equal 10, extract("Fix #10")
    assert_equal 20, extract("Close #20")
    assert_equal 30, extract("Resolve #30")
  end

  test "case insensitive - uppercase FIXES" do
    assert_equal 123, extract("FIXES #123")
  end

  test "case insensitive - lowercase fixes" do
    assert_equal 123, extract("fixes #123")
  end

  test "case insensitive - mixed case Fixes" do
    assert_equal 123, extract("Fixes #123")
  end

  test "case insensitive - mixed case cLoSeS" do
    assert_equal 456, extract("cLoSeS #456")
  end

  test "returns nil when no linked issue found" do
    assert_nil extract("This PR improves performance")
  end

  test "returns nil for empty body" do
    assert_nil extract("")
  end

  test "returns nil for nil body" do
    assert_nil extract(nil)
  end

  test "returns first issue when multiple linked issues present" do
    body = "Fixes #100\nCloses #200\nResolves #300"
    assert_equal 100, extract(body)
  end

  test "deduplicates issue numbers before returning first" do
    body = "Fixes #100\nCloses #100"
    assert_equal 100, extract(body)
  end

  test "extracts issue from multiline PR body with other content" do
    body = <<~BODY
      ## Summary
      This PR adds a new feature.

      Fixes #42

      ## Test Plan
      - Run the tests
    BODY
    assert_equal 42, extract(body)
  end

  test "does not match bare issue references without keyword" do
    assert_nil extract("Related to #123")
  end

  test "keyword mid-word still matches because regex lacks word boundary" do
    # NOTE: The regex does not enforce word boundaries, so "prefix" contains "fix"
    # and will match. This documents the current behavior. If word boundaries are
    # desired, the regex should be updated to use \b.
    assert_equal 123, extract("prefix #123")
  end

  # === Full call flow with stubs ===

  test "call returns issue number when PR body contains linked issue" do
    stub_github_api(pr_body: "Fixes #99") do
      result = @resolver.call
      assert_equal 99, result
    end
  end

  test "call returns nil when installation not found" do
    Github::App.stub :installation_id_for_repo, nil do
      result = @resolver.call
      assert_nil result
    end
  end

  test "call returns nil when installation token fails" do
    Github::App.stub :installation_id_for_repo, 111 do
      Github::App.stub :installation_client, ->(*) { raise "Failed to get installation token: bad token" } do
        result = @resolver.call
        assert_nil result
      end
    end
  end

  test "call returns nil when PR body is blank" do
    stub_github_api(pr_body: "") do
      result = @resolver.call
      assert_nil result
    end
  end

  test "call returns nil when PR body has no linking keywords" do
    stub_github_api(pr_body: "Just some refactoring, no linked issues") do
      result = @resolver.call
      assert_nil result
    end
  end

  test "call returns nil and does not raise on API error" do
    Github::App.stub :installation_id_for_repo, ->(*) { raise StandardError, "boom" } do
      result = @resolver.call
      assert_nil result
    end
  end

  private

  # Directly test the regex extraction without hitting any APIs.
  def extract(pr_body)
    @resolver.send(:extract_first_linked_issue, pr_body)
  end

  # Stubs the GitHub API chain: installation lookup and client.
  def stub_github_api(pr_body:)
    pr_double = OpenStruct.new(body: pr_body)
    client_double = Minitest::Mock.new
    client_double.expect(:pull_request, pr_double, [ @repo, @pr_number ])

    Github::App.stub :installation_id_for_repo, 111 do
      Github::App.stub :installation_client, client_double do
        yield
      end
    end

    client_double.verify
  end
end
