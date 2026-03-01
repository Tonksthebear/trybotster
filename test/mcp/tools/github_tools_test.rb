# frozen_string_literal: true

require "test_helper"
require "minitest/mock"
require "ostruct"

# Test helper for MCP tools
module MCPToolTestHelper
  extend ActiveSupport::Concern

  included do
    fixtures :users
  end

  # ISO8601 date pattern for parsing
  ISO8601_PATTERN = /\A\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z?\z/

  # Load a JSON fixture file and convert to Octokit-like format
  def load_github_fixture(name)
    data = JSON.parse(File.read(Rails.root.join("test/fixtures/github/#{name}.json")))
    convert_to_octokit_format(data)
  end

  # Convert JSON data to Octokit-like format:
  # - Use HashWithIndifferentAccess for string/symbol key access
  # - Parse ISO8601 dates to Time objects
  def convert_to_octokit_format(obj)
    case obj
    when Hash
      result = obj.transform_values { |v| convert_to_octokit_format(v) }
      result.with_indifferent_access
    when Array
      obj.map { |v| convert_to_octokit_format(v) }
    when String
      # Parse ISO8601 date strings to Time objects (like Octokit does)
      if obj.match?(ISO8601_PATTERN)
        Time.parse(obj)
      else
        obj
      end
    else
      obj
    end
  end

  # Alias for backward compatibility
  def symbolize_keys_deep(obj)
    convert_to_octokit_format(obj)
  end

  # Create a mock Sawyer::Resource-like object
  def mock_resource(data)
    resource = OpenStruct.new(data)
    resource.define_singleton_method(:to_h) { data }
    resource
  end

  # Setup tool with common mocks
  def setup_tool_mocks(tool)
    tool.define_singleton_method(:render) { |**args| @rendered = args }
    tool.define_singleton_method(:report_error) { |msg| @error = msg }
    tool.define_singleton_method(:detect_client_type) { "Test Client" }
    tool.define_singleton_method(:attribution_footer) { "\n\n_via test_" }
    tool
  end

  # Stub installation lookup + client for a block
  def with_installation_client(mock_client, installation_id: 12345, &block)
    Github::App.stub :installation_id_for_repo, installation_id do
      Github::App.stub :installation_client, mock_client do
        block.call
      end
    end
  end

  # Stub installation_id_for_repo returning nil (app not installed)
  def with_no_installation(&block)
    Github::App.stub :installation_id_for_repo, nil do
      block.call
    end
  end

  # Create a mock Octokit client that responds to a method with fixture data
  def mock_octokit_client(method_name, return_data)
    client = Object.new
    client.define_singleton_method(method_name) { |*_args, **_kwargs| return_data }
    client
  end
end

class GithubGetPullRequestToolTest < ActiveSupport::TestCase
  include MCPToolTestHelper

  setup do
    @pr_data = symbolize_keys_deep(load_github_fixture("pull_request"))
  end

  test "returns pull request details on success" do
    tool = GithubGetPullRequestTool.new(repo: "owner/repo", pr_number: 49)
    setup_tool_mocks(tool)

    client = mock_octokit_client(:pull_request, mock_resource(@pr_data))
    with_installation_client(client) do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
      rendered = tool.instance_variable_get(:@rendered)
      assert rendered[:text]&.include?("Pull Request #49")
    end
  end

  test "returns error when app not installed" do
    tool = GithubGetPullRequestTool.new(repo: "owner/repo", pr_number: 49)
    setup_tool_mocks(tool)

    with_no_installation do
      tool.perform
      error = tool.instance_variable_get(:@error)
      assert error&.include?("not installed")
    end
  end

  test "validates repo format" do
    tool = GithubGetPullRequestTool.new(repo: "invalid-repo", pr_number: 49)
    assert_not tool.valid?
    assert_includes tool.errors[:repo], "must be in 'owner/repo' format"
  end

  test "validates pr_number is positive integer" do
    tool = GithubGetPullRequestTool.new(repo: "owner/repo", pr_number: 0)
    assert_not tool.valid?
    assert_includes tool.errors[:pr_number], "must be greater than 0"
  end
end

class GithubGetIssueToolTest < ActiveSupport::TestCase
  include MCPToolTestHelper

  setup do
    @issue_data = symbolize_keys_deep(load_github_fixture("issue"))
  end

  test "returns issue details on success" do
    tool = GithubGetIssueTool.new(repo: "owner/repo", issue_number: 123)
    setup_tool_mocks(tool)

    client = mock_octokit_client(:issue, mock_resource(@issue_data))
    with_installation_client(client) do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
    end
  end

  test "returns error when app not installed" do
    tool = GithubGetIssueTool.new(repo: "owner/repo", issue_number: 123)
    setup_tool_mocks(tool)

    with_no_installation do
      tool.perform
      error = tool.instance_variable_get(:@error)
      assert error&.include?("not installed")
    end
  end

  test "validates repo format" do
    tool = GithubGetIssueTool.new(repo: "invalid", issue_number: 123)
    assert_not tool.valid?
  end

  test "validates issue_number is positive" do
    tool = GithubGetIssueTool.new(repo: "owner/repo", issue_number: -1)
    assert_not tool.valid?
  end
end

class GithubGetIssueCommentsToolTest < ActiveSupport::TestCase
  include MCPToolTestHelper

  setup do
    @comments = [ mock_resource(symbolize_keys_deep(load_github_fixture("comment"))) ]
  end

  test "returns comments on success" do
    tool = GithubGetIssueCommentsTool.new(repo: "owner/repo", issue_number: 123)
    setup_tool_mocks(tool)

    client = mock_octokit_client(:issue_comments, @comments)
    with_installation_client(client) do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
    end
  end

  test "handles empty comments" do
    tool = GithubGetIssueCommentsTool.new(repo: "owner/repo", issue_number: 123)
    setup_tool_mocks(tool)

    client = mock_octokit_client(:issue_comments, [])
    with_installation_client(client) do
      tool.perform
      rendered = tool.instance_variable_get(:@rendered)
      assert rendered[:text]&.include?("No comments found")
    end
  end
end

class GithubCreateIssueToolTest < ActiveSupport::TestCase
  include MCPToolTestHelper

  setup do
    @created_issue = symbolize_keys_deep(load_github_fixture("issue"))
  end

  test "creates issue on success" do
    tool = GithubCreateIssueTool.new(repo: "owner/repo", title: "New issue", body: "Description")
    setup_tool_mocks(tool)

    client = mock_octokit_client(:create_issue, mock_resource(@created_issue))
    with_installation_client(client) do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
    end
  end

  test "returns error when app not installed" do
    tool = GithubCreateIssueTool.new(repo: "owner/repo", title: "New issue", body: "Description")
    setup_tool_mocks(tool)

    with_no_installation do
      tool.perform
      error = tool.instance_variable_get(:@error)
      assert error&.include?("not installed")
    end
  end

  test "validates title presence" do
    tool = GithubCreateIssueTool.new(repo: "owner/repo", title: "", body: "Description")
    assert_not tool.valid?
    assert tool.errors[:title].any?
  end

  test "validates title length" do
    tool = GithubCreateIssueTool.new(repo: "owner/repo", title: "a" * 300, body: "Description")
    assert_not tool.valid?
    assert tool.errors[:title].any?
  end
end

class GithubCommentIssueToolTest < ActiveSupport::TestCase
  include MCPToolTestHelper

  setup do
    @comment = symbolize_keys_deep(load_github_fixture("comment"))
  end

  test "adds comment on success" do
    tool = GithubCommentIssueTool.new(repo: "owner/repo", issue_number: 123, body: "Great work!")
    setup_tool_mocks(tool)

    client = mock_octokit_client(:add_comment, mock_resource(@comment))
    with_installation_client(client) do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
    end
  end

  test "returns error when app not installed" do
    tool = GithubCommentIssueTool.new(repo: "owner/repo", issue_number: 123, body: "Great work!")
    setup_tool_mocks(tool)

    with_no_installation do
      tool.perform
      error = tool.instance_variable_get(:@error)
      assert error&.include?("not installed")
    end
  end

  test "validates body presence" do
    tool = GithubCommentIssueTool.new(repo: "owner/repo", issue_number: 123, body: "")
    assert_not tool.valid?
  end

  test "returns cached response when idempotency key exists and completed" do
    # Create a completed idempotency key with cached response
    cached_response = {
      success: true,
      text: "✅ Comment added successfully!\n\n💬 Comment on owner/repo#123\n   URL: https://github.com/owner/repo/issues/123#issuecomment-999"
    }.to_json

    idempotency_key = IdempotencyKey.create!(
      key: "test-idempotency-key",
      request_path: "github_comment_issue",
      request_params: { repo: "owner/repo", issue_number: 123, body: "Great work!" }.to_json,
      response_body: cached_response,
      response_status: 200,
      completed_at: Time.current
    )

    tool = GithubCommentIssueTool.new(repo: "owner/repo", issue_number: 123, body: "Great work!")
    setup_tool_mocks(tool)

    # Mock the idempotency key header
    tool.define_singleton_method(:idempotency_key_from_request) { "test-idempotency-key" }

    # The API should NOT be called since we have a cached response
    Github::App.stub :installation_id_for_repo, ->(*) { raise "API should not be called" } do
      tool.perform
      rendered = tool.instance_variable_get(:@rendered)
      assert rendered[:text]&.include?("Comment added successfully")
    end
  end

  test "stores response in idempotency key after successful execution" do
    tool = GithubCommentIssueTool.new(repo: "owner/repo", issue_number: 123, body: "New comment")
    setup_tool_mocks(tool)

    idempotency_key_value = "new-idempotency-key-#{SecureRandom.hex(8)}"
    tool.define_singleton_method(:idempotency_key_from_request) { idempotency_key_value }

    client = mock_octokit_client(:add_comment, mock_resource(@comment))
    with_installation_client(client) do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
    end

    # Verify idempotency key was stored
    stored_key = IdempotencyKey.find_by(key: idempotency_key_value)
    assert stored_key.present?, "Idempotency key should be stored"
    assert stored_key.completed?, "Idempotency key should be marked completed"
    assert_equal 200, stored_key.response_status
  end

  test "does not duplicate comment on retry with same idempotency key" do
    idempotency_key_value = "retry-key-#{SecureRandom.hex(8)}"
    api_call_count = 0

    comment_data = @comment
    mock_client = Object.new
    mock_client.define_singleton_method(:add_comment) do |*_args|
      api_call_count += 1
      resource = OpenStruct.new(comment_data)
      resource.define_singleton_method(:to_h) { comment_data }
      resource
    end

    # First request - should call API
    tool1 = GithubCommentIssueTool.new(repo: "owner/repo", issue_number: 123, body: "Retry test")
    setup_tool_mocks(tool1)
    tool1.define_singleton_method(:idempotency_key_from_request) { idempotency_key_value }

    with_installation_client(mock_client) do
      tool1.perform
    end

    assert_equal 1, api_call_count, "First request should call API once"

    # Second request with same key - should NOT call API
    tool2 = GithubCommentIssueTool.new(repo: "owner/repo", issue_number: 123, body: "Retry test")
    setup_tool_mocks(tool2)
    tool2.define_singleton_method(:idempotency_key_from_request) { idempotency_key_value }

    with_installation_client(mock_client) do
      tool2.perform
    end

    assert_equal 1, api_call_count, "Second request with same idempotency key should NOT call API again"
  end

  test "proceeds with API call when no idempotency key provided" do
    tool = GithubCommentIssueTool.new(repo: "owner/repo", issue_number: 123, body: "No key test")
    setup_tool_mocks(tool)
    tool.define_singleton_method(:idempotency_key_from_request) { nil }

    api_called = false
    comment_data = @comment
    mock_client = Object.new
    mock_client.define_singleton_method(:add_comment) do |*_args|
      api_called = true
      resource = OpenStruct.new(comment_data)
      resource.define_singleton_method(:to_h) { comment_data }
      resource
    end

    with_installation_client(mock_client) do
      tool.perform
    end

    assert api_called, "API should be called when no idempotency key is provided"
  end
end

class GithubUpdateIssueToolTest < ActiveSupport::TestCase
  include MCPToolTestHelper

  setup do
    @updated_issue = symbolize_keys_deep(load_github_fixture("issue"))
  end

  test "updates issue on success" do
    tool = GithubUpdateIssueTool.new(repo: "owner/repo", issue_number: 123, state: "closed")
    setup_tool_mocks(tool)

    client = mock_octokit_client(:update_issue, mock_resource(@updated_issue))
    with_installation_client(client) do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
    end
  end

  test "returns error when app not installed" do
    tool = GithubUpdateIssueTool.new(repo: "owner/repo", issue_number: 123, state: "closed")
    setup_tool_mocks(tool)

    with_no_installation do
      tool.perform
      error = tool.instance_variable_get(:@error)
      assert error&.include?("not installed")
    end
  end

  test "validates state must be open or closed" do
    tool = GithubUpdateIssueTool.new(repo: "owner/repo", issue_number: 123, state: "invalid")
    assert_not tool.valid?
    assert_includes tool.errors[:state], "must be 'open' or 'closed'"
  end

  test "requires at least one update parameter" do
    tool = GithubUpdateIssueTool.new(repo: "owner/repo", issue_number: 123)
    setup_tool_mocks(tool)

    client = Object.new
    with_installation_client(client) do
      tool.perform
      error = tool.instance_variable_get(:@error)
      assert error&.include?("No update parameters")
    end
  end
end

class GithubCreatePullRequestToolTest < ActiveSupport::TestCase
  include MCPToolTestHelper

  setup do
    @created_pr = symbolize_keys_deep(load_github_fixture("pull_request"))
  end

  test "creates pull request on success" do
    tool = GithubCreatePullRequestTool.new(
      repo: "owner/repo",
      title: "New feature",
      head: "feature-branch",
      base: "main",
      body: "Description"
    )
    setup_tool_mocks(tool)

    client = mock_octokit_client(:create_pull_request, mock_resource(@created_pr))
    with_installation_client(client) do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
    end
  end

  test "returns error when app not installed" do
    tool = GithubCreatePullRequestTool.new(
      repo: "owner/repo",
      title: "New feature",
      head: "feature-branch",
      base: "main"
    )
    setup_tool_mocks(tool)

    with_no_installation do
      tool.perform
      error = tool.instance_variable_get(:@error)
      assert error&.include?("not installed")
    end
  end

  test "validates head branch presence" do
    tool = GithubCreatePullRequestTool.new(repo: "owner/repo", title: "PR", head: "", base: "main")
    assert_not tool.valid?
  end

  test "validates base branch presence" do
    tool = GithubCreatePullRequestTool.new(repo: "owner/repo", title: "PR", head: "feature", base: "")
    assert_not tool.valid?
  end
end

class GithubListIssuesToolTest < ActiveSupport::TestCase
  include MCPToolTestHelper

  setup do
    @issues = [ mock_resource(symbolize_keys_deep(load_github_fixture("issue"))) ]
  end

  test "lists issues on success" do
    tool = GithubListIssuesTool.new(repo: "owner/repo", state: "open")
    setup_tool_mocks(tool)

    client = mock_octokit_client(:list_issues, @issues)
    with_installation_client(client) do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
    end
  end

  test "handles empty issues list" do
    tool = GithubListIssuesTool.new(repo: "owner/repo")
    setup_tool_mocks(tool)

    client = mock_octokit_client(:list_issues, [])
    with_installation_client(client) do
      tool.perform
      rendered = tool.instance_variable_get(:@rendered)
      assert rendered[:text]&.include?("No issues found")
    end
  end

  test "returns error when app not installed" do
    tool = GithubListIssuesTool.new(repo: "owner/repo")
    setup_tool_mocks(tool)

    with_no_installation do
      tool.perform
      error = tool.instance_variable_get(:@error)
      assert error&.include?("not installed")
    end
  end

  test "validates repo format" do
    tool = GithubListIssuesTool.new(repo: "invalid")
    assert_not tool.valid?
  end
end

class GithubListReposToolTest < ActiveSupport::TestCase
  include MCPToolTestHelper

  setup do
    @repos = [ symbolize_keys_deep(load_github_fixture("repository")) ]
  end

  test "lists repositories on success" do
    tool = GithubListReposTool.new(per_page: 10, sort: "updated_at")
    setup_tool_mocks(tool)

    Github::App.stub :list_installation_repos, @repos do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
    end
  end

  test "handles empty repos list" do
    tool = GithubListReposTool.new
    setup_tool_mocks(tool)

    Github::App.stub :list_installation_repos, [] do
      tool.perform
      rendered = tool.instance_variable_get(:@rendered)
      assert rendered[:text]&.include?("No repositories found")
    end
  end
end

class GithubSearchReposToolTest < ActiveSupport::TestCase
  include MCPToolTestHelper

  setup do
    @repos = [ mock_resource(symbolize_keys_deep(load_github_fixture("repository"))) ]
    @search_result = OpenStruct.new(items: @repos, total_count: 1)
  end

  test "searches repositories on success" do
    tool = GithubSearchReposTool.new(query: "rails language:ruby", sort: "stars")
    setup_tool_mocks(tool)

    client = mock_octokit_client(:search_repositories, @search_result)

    Github::App.stub :installation_id_for_repo, nil do
      Github::App.stub :first_installation_id, 12345 do
        Github::App.stub :installation_client, client do
          tool.perform
          assert_nil tool.instance_variable_get(:@error)
        end
      end
    end
  end

  test "validates query presence" do
    tool = GithubSearchReposTool.new(query: "")
    assert_not tool.valid?
  end

  test "handles no results" do
    tool = GithubSearchReposTool.new(query: "nonexistent-repo-xyz")
    setup_tool_mocks(tool)

    empty_result = OpenStruct.new(items: [], total_count: 0)
    client = mock_octokit_client(:search_repositories, empty_result)

    Github::App.stub :installation_id_for_repo, nil do
      Github::App.stub :first_installation_id, 12345 do
        Github::App.stub :installation_client, client do
          tool.perform
          rendered = tool.instance_variable_get(:@rendered)
          assert rendered[:text]&.include?("No repositories found")
        end
      end
    end
  end
end

class GithubGetPullRequestFilesToolTest < ActiveSupport::TestCase
  include MCPToolTestHelper

  setup do
    raw = JSON.parse(File.read(Rails.root.join("test/fixtures/github/pull_request_files.json")))
    @files = raw.map { |f| mock_resource(convert_to_octokit_format(f)) }
  end

  test "returns changed file manifest on success" do
    tool = GithubGetPullRequestFilesTool.new(repo: "owner/repo", pr_number: 49)
    setup_tool_mocks(tool)

    client = mock_octokit_client(:pull_request_files, @files)
    with_installation_client(client) do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
      rendered = tool.instance_variable_get(:@rendered)
      assert rendered[:text]&.include?("Changed files in owner/repo#49")
      assert rendered[:text]&.include?("app/models/user.rb")
      assert rendered[:text]&.include?("app/controllers/users_controller.rb")
      assert rendered[:text]&.include?("3 files")
      # No patch content — agent reads files locally
      assert_not rendered[:text]&.include?("@@")
    end
  end

  test "shows per-file change counts and totals" do
    tool = GithubGetPullRequestFilesTool.new(repo: "owner/repo", pr_number: 49)
    setup_tool_mocks(tool)

    client = mock_octokit_client(:pull_request_files, @files)
    with_installation_client(client) do
      tool.perform
      rendered = tool.instance_variable_get(:@rendered)
      # Per-file stats
      assert rendered[:text]&.include?("+12")
      assert rendered[:text]&.include?("+40")
      # Totals line
      assert rendered[:text]&.include?("Total:")
    end
  end

  test "handles empty file list" do
    tool = GithubGetPullRequestFilesTool.new(repo: "owner/repo", pr_number: 49)
    setup_tool_mocks(tool)

    client = mock_octokit_client(:pull_request_files, [])
    with_installation_client(client) do
      tool.perform
      rendered = tool.instance_variable_get(:@rendered)
      assert rendered[:text]&.include?("No changed files found")
    end
  end

  test "returns error when app not installed" do
    tool = GithubGetPullRequestFilesTool.new(repo: "owner/repo", pr_number: 49)
    setup_tool_mocks(tool)

    with_no_installation do
      tool.perform
      error = tool.instance_variable_get(:@error)
      assert error&.include?("not installed")
    end
  end

  test "validates repo format" do
    tool = GithubGetPullRequestFilesTool.new(repo: "bad-format", pr_number: 49)
    assert_not tool.valid?
    assert_includes tool.errors[:repo], "must be in 'owner/repo' format"
  end

  test "validates pr_number is positive" do
    tool = GithubGetPullRequestFilesTool.new(repo: "owner/repo", pr_number: 0)
    assert_not tool.valid?
    assert tool.errors[:pr_number].any?
  end
end

class GithubCreatePullRequestReviewToolTest < ActiveSupport::TestCase
  include MCPToolTestHelper

  setup do
    @review_data = symbolize_keys_deep(load_github_fixture("pull_request_review"))
  end

  test "submits APPROVE review on success" do
    tool = GithubCreatePullRequestReviewTool.new(
      repo: "owner/repo",
      pr_number: 49,
      event: "APPROVE",
      body: "Looks great!"
    )
    setup_tool_mocks(tool)

    client = mock_octokit_client(:create_pull_request_review, mock_resource(@review_data))
    with_installation_client(client) do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
      rendered = tool.instance_variable_get(:@rendered)
      assert rendered[:text]&.include?("Approved")
      assert rendered[:text]&.include?("owner/repo#49")
    end
  end

  test "submits REQUEST_CHANGES review on success" do
    tool = GithubCreatePullRequestReviewTool.new(
      repo: "owner/repo",
      pr_number: 49,
      event: "REQUEST_CHANGES",
      body: "Please fix the error handling."
    )
    setup_tool_mocks(tool)

    review = mock_resource(@review_data.merge(state: "CHANGES_REQUESTED"))
    client = mock_octokit_client(:create_pull_request_review, review)
    with_installation_client(client) do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
      rendered = tool.instance_variable_get(:@rendered)
      assert rendered[:text]&.include?("Changes requested")
    end
  end

  test "submits COMMENT review with inline comments" do
    tool = GithubCreatePullRequestReviewTool.new(
      repo: "owner/repo",
      pr_number: 49,
      event: "COMMENT",
      body: "Some thoughts inline.",
      comments: [
        { "path" => "app/models/user.rb", "line" => 15, "body" => "Consider using a scope here." },
        { "path" => "app/controllers/users_controller.rb", "line" => 3, "body" => "Missing authorization check." }
      ]
    )
    setup_tool_mocks(tool)

    client = mock_octokit_client(:create_pull_request_review, mock_resource(@review_data))
    with_installation_client(client) do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
      rendered = tool.instance_variable_get(:@rendered)
      assert rendered[:text]&.include?("Inline comments: 2")
    end
  end

  test "returns error when app not installed" do
    tool = GithubCreatePullRequestReviewTool.new(
      repo: "owner/repo", pr_number: 49, event: "APPROVE"
    )
    setup_tool_mocks(tool)

    with_no_installation do
      tool.perform
      error = tool.instance_variable_get(:@error)
      assert error&.include?("not installed")
    end
  end

  test "validates repo format" do
    tool = GithubCreatePullRequestReviewTool.new(repo: "bad", pr_number: 49, event: "APPROVE")
    assert_not tool.valid?
    assert_includes tool.errors[:repo], "must be in 'owner/repo' format"
  end

  test "validates event is one of the allowed values" do
    tool = GithubCreatePullRequestReviewTool.new(repo: "owner/repo", pr_number: 49, event: "MERGE")
    assert_not tool.valid?
    assert_includes tool.errors[:event], "must be 'APPROVE', 'REQUEST_CHANGES', or 'COMMENT'"
  end

  test "validates body is required for REQUEST_CHANGES" do
    tool = GithubCreatePullRequestReviewTool.new(repo: "owner/repo", pr_number: 49, event: "REQUEST_CHANGES")
    assert_not tool.valid?
    assert_includes tool.errors[:body], "is required when requesting changes"
  end

  test "allows APPROVE without body" do
    tool = GithubCreatePullRequestReviewTool.new(repo: "owner/repo", pr_number: 49, event: "APPROVE")
    assert tool.valid?
  end

  test "validates comment items have required fields" do
    tool = GithubCreatePullRequestReviewTool.new(
      repo: "owner/repo",
      pr_number: 49,
      event: "COMMENT",
      comments: [ { "path" => "app/models/user.rb" } ]
    )
    assert_not tool.valid?
    assert tool.errors[:comments].any? { |e| e.include?("missing 'line'") }
    assert tool.errors[:comments].any? { |e| e.include?("missing 'body'") }
  end

  test "validates comment line must be positive" do
    tool = GithubCreatePullRequestReviewTool.new(
      repo: "owner/repo",
      pr_number: 49,
      event: "COMMENT",
      comments: [ { "path" => "app/models/user.rb", "line" => 0, "body" => "nit" } ]
    )
    assert_not tool.valid?
    assert tool.errors[:comments].any? { |e| e.include?("positive integer") }
  end

  test "returns cached response when idempotency key exists" do
    cached_response = { success: true, text: "✅ Approved — owner/repo#49\n\n🔗 Review URL: https://github.com/owner/repo/pull/49#review-123" }.to_json

    IdempotencyKey.create!(
      key: "review-idempotency-key",
      request_path: "github_create_pull_request_review",
      request_params: { repo: "owner/repo", pr_number: 49, event: "APPROVE" }.to_json,
      response_body: cached_response,
      response_status: 200,
      completed_at: Time.current
    )

    tool = GithubCreatePullRequestReviewTool.new(repo: "owner/repo", pr_number: 49, event: "APPROVE")
    setup_tool_mocks(tool)
    tool.define_singleton_method(:idempotency_key_from_request) { "review-idempotency-key" }

    Github::App.stub :installation_id_for_repo, ->(*) { raise "API should not be called" } do
      tool.perform
      rendered = tool.instance_variable_get(:@rendered)
      assert rendered[:text]&.include?("Approved")
    end
  end

  test "stores idempotency response after successful review" do
    tool = GithubCreatePullRequestReviewTool.new(
      repo: "owner/repo", pr_number: 49, event: "APPROVE", body: "LGTM"
    )
    setup_tool_mocks(tool)

    key_value = "review-store-key-#{SecureRandom.hex(8)}"
    tool.define_singleton_method(:idempotency_key_from_request) { key_value }

    client = mock_octokit_client(:create_pull_request_review, mock_resource(@review_data))
    with_installation_client(client) do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
    end

    stored = IdempotencyKey.find_by(key: key_value)
    assert stored.present?, "Idempotency key should be stored"
    assert stored.completed?, "Idempotency key should be marked completed"
  end

  test "does not submit duplicate review on retry with same idempotency key" do
    key_value = "review-retry-key-#{SecureRandom.hex(8)}"
    api_call_count = 0
    review_data = @review_data

    mock_client = Object.new
    mock_client.define_singleton_method(:create_pull_request_review) do |*_args, **_kwargs|
      api_call_count += 1
      resource = OpenStruct.new(review_data)
      resource.define_singleton_method(:to_h) { review_data }
      resource
    end

    tool1 = GithubCreatePullRequestReviewTool.new(repo: "owner/repo", pr_number: 49, event: "APPROVE")
    setup_tool_mocks(tool1)
    tool1.define_singleton_method(:idempotency_key_from_request) { key_value }
    with_installation_client(mock_client) { tool1.perform }

    assert_equal 1, api_call_count

    tool2 = GithubCreatePullRequestReviewTool.new(repo: "owner/repo", pr_number: 49, event: "APPROVE")
    setup_tool_mocks(tool2)
    tool2.define_singleton_method(:idempotency_key_from_request) { key_value }
    with_installation_client(mock_client) { tool2.perform }

    assert_equal 1, api_call_count, "Retry should not call the GitHub API again"
  end
end
