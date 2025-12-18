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

  # Create a mock authorized user (avoids encryption issues)
  def create_authorized_user
    user = OpenStruct.new(
      id: 1,
      email: "test@example.com",
      username: "testuser"
    )
    user.define_singleton_method(:github_app_authorized?) { true }
    user.define_singleton_method(:valid_github_app_token) { "mock_token_123" }
    user
  end

  # Create a mock unauthorized user
  def create_unauthorized_user
    user = OpenStruct.new(
      id: 2,
      email: "test2@example.com",
      username: "testuser2"
    )
    user.define_singleton_method(:github_app_authorized?) { false }
    user.define_singleton_method(:valid_github_app_token) { nil }
    user
  end

  # Setup tool with common mocks
  def setup_tool_mocks(tool, user: nil)
    user ||= create_authorized_user
    tool.define_singleton_method(:current_user) { user }
    tool.define_singleton_method(:render) { |**args| @rendered = args }
    tool.define_singleton_method(:report_error) { |msg| @error = msg }
    tool.define_singleton_method(:detect_client_type) { "Test Client" }
    tool
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

    Github::App.stub :get_pull_request, { success: true, pull_request: @pr_data } do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
      rendered = tool.instance_variable_get(:@rendered)
      assert rendered[:text]&.include?("Pull Request #49")
    end
  end

  test "returns error when user not authorized" do
    tool = GithubGetPullRequestTool.new(repo: "owner/repo", pr_number: 49)
    setup_tool_mocks(tool, user: create_unauthorized_user)

    tool.perform
    error = tool.instance_variable_get(:@error)
    assert error&.include?("not authorized")
  end

  test "returns error when API call fails" do
    tool = GithubGetPullRequestTool.new(repo: "owner/repo", pr_number: 49)
    setup_tool_mocks(tool)

    Github::App.stub :get_pull_request, { success: false, error: "Not found" } do
      tool.perform
      error = tool.instance_variable_get(:@error)
      assert error&.include?("Not found")
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

    Github::App.stub :get_issue, { success: true, issue: @issue_data } do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
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
    @comments = [ symbolize_keys_deep(load_github_fixture("comment")) ]
  end

  test "returns comments on success" do
    tool = GithubGetIssueCommentsTool.new(repo: "owner/repo", issue_number: 123)
    setup_tool_mocks(tool)

    Github::App.stub :get_issue_comments, { success: true, comments: @comments } do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
    end
  end

  test "handles empty comments" do
    tool = GithubGetIssueCommentsTool.new(repo: "owner/repo", issue_number: 123)
    setup_tool_mocks(tool)

    Github::App.stub :get_issue_comments, { success: true, comments: [] } do
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

    installation_result = { success: true, installation_id: 12345, account: "owner" }
    mock_client = Object.new
    created_issue = @created_issue
    mock_client.define_singleton_method(:create_issue) { |*args| mock_resource(created_issue) }

    # Need mock_resource accessible in the mock_client context
    def mock_client.mock_resource(data)
      resource = OpenStruct.new(data)
      resource.define_singleton_method(:to_h) { data }
      resource
    end

    Github::App.stub :get_installation_for_repo, installation_result do
      Github::App.stub :installation_client, mock_client do
        tool.perform
        assert_nil tool.instance_variable_get(:@error)
      end
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

    installation_result = { success: true, installation_id: 12345, account: "owner" }
    comment_data = @comment
    mock_client = Object.new
    mock_client.define_singleton_method(:add_comment) do |*args|
      resource = OpenStruct.new(comment_data)
      resource.define_singleton_method(:to_h) { comment_data }
      resource
    end

    Github::App.stub :get_installation_for_repo, installation_result do
      Github::App.stub :installation_client, mock_client do
        tool.perform
        assert_nil tool.instance_variable_get(:@error)
      end
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
    Github::App.stub :get_installation_for_repo, ->(*) { raise "API should not be called" } do
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

    installation_result = { success: true, installation_id: 12345, account: "owner" }
    comment_data = @comment
    mock_client = Object.new
    mock_client.define_singleton_method(:add_comment) do |*args|
      resource = OpenStruct.new(comment_data)
      resource.define_singleton_method(:to_h) { comment_data }
      resource
    end

    Github::App.stub :get_installation_for_repo, installation_result do
      Github::App.stub :installation_client, mock_client do
        tool.perform
        assert_nil tool.instance_variable_get(:@error)
      end
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

    installation_result = { success: true, installation_id: 12345, account: "owner" }
    comment_data = @comment
    mock_client = Object.new
    mock_client.define_singleton_method(:add_comment) do |*args|
      api_call_count += 1
      resource = OpenStruct.new(comment_data)
      resource.define_singleton_method(:to_h) { comment_data }
      resource
    end

    # First request - should call API
    tool1 = GithubCommentIssueTool.new(repo: "owner/repo", issue_number: 123, body: "Retry test")
    setup_tool_mocks(tool1)
    tool1.define_singleton_method(:idempotency_key_from_request) { idempotency_key_value }

    Github::App.stub :get_installation_for_repo, installation_result do
      Github::App.stub :installation_client, mock_client do
        tool1.perform
      end
    end

    assert_equal 1, api_call_count, "First request should call API once"

    # Second request with same key - should NOT call API
    tool2 = GithubCommentIssueTool.new(repo: "owner/repo", issue_number: 123, body: "Retry test")
    setup_tool_mocks(tool2)
    tool2.define_singleton_method(:idempotency_key_from_request) { idempotency_key_value }

    Github::App.stub :get_installation_for_repo, installation_result do
      Github::App.stub :installation_client, mock_client do
        tool2.perform
      end
    end

    assert_equal 1, api_call_count, "Second request with same idempotency key should NOT call API again"
  end

  test "proceeds with API call when no idempotency key provided" do
    tool = GithubCommentIssueTool.new(repo: "owner/repo", issue_number: 123, body: "No key test")
    setup_tool_mocks(tool)
    tool.define_singleton_method(:idempotency_key_from_request) { nil }

    installation_result = { success: true, installation_id: 12345, account: "owner" }
    comment_data = @comment
    mock_client = Object.new
    api_called = false
    mock_client.define_singleton_method(:add_comment) do |*args|
      api_called = true
      resource = OpenStruct.new(comment_data)
      resource.define_singleton_method(:to_h) { comment_data }
      resource
    end

    Github::App.stub :get_installation_for_repo, installation_result do
      Github::App.stub :installation_client, mock_client do
        tool.perform
      end
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

    installation_result = { success: true, installation_id: 12345, account: "owner" }
    issue_data = @updated_issue
    mock_client = Object.new
    mock_client.define_singleton_method(:update_issue) do |*args|
      resource = OpenStruct.new(issue_data)
      resource.define_singleton_method(:to_h) { issue_data }
      resource
    end

    Github::App.stub :get_installation_for_repo, installation_result do
      Github::App.stub :installation_client, mock_client do
        tool.perform
        assert_nil tool.instance_variable_get(:@error)
      end
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

    installation_result = { success: true, installation_id: 12345, account: "owner" }
    mock_client = Object.new

    Github::App.stub :get_installation_for_repo, installation_result do
      Github::App.stub :installation_client, mock_client do
        tool.perform
        error = tool.instance_variable_get(:@error)
        assert error&.include?("No update parameters")
      end
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

    installation_result = { success: true, installation_id: 12345, account: "owner" }
    pr_data = @created_pr
    mock_client = Object.new
    mock_client.define_singleton_method(:create_pull_request) do |*args|
      resource = OpenStruct.new(pr_data)
      resource.define_singleton_method(:to_h) { pr_data }
      resource
    end

    Github::App.stub :get_installation_for_repo, installation_result do
      Github::App.stub :installation_client, mock_client do
        tool.perform
        assert_nil tool.instance_variable_get(:@error)
      end
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
    @issues = [ symbolize_keys_deep(load_github_fixture("issue")) ]
  end

  test "lists issues on success" do
    tool = GithubListIssuesTool.new(filter: "assigned", state: "open")
    setup_tool_mocks(tool)

    Github::App.stub :get_user_issues, { success: true, issues: @issues } do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
    end
  end

  test "handles empty issues list" do
    tool = GithubListIssuesTool.new
    setup_tool_mocks(tool)

    Github::App.stub :get_user_issues, { success: true, issues: [] } do
      tool.perform
      rendered = tool.instance_variable_get(:@rendered)
      assert rendered[:text]&.include?("No issues found")
    end
  end
end

class GithubListReposToolTest < ActiveSupport::TestCase
  include MCPToolTestHelper

  setup do
    @repos = [ symbolize_keys_deep(load_github_fixture("repository")) ]
  end

  test "lists repositories on success" do
    tool = GithubListReposTool.new(per_page: 10, sort: "updated")
    setup_tool_mocks(tool)

    Github::App.stub :get_user_repos, { success: true, repos: @repos } do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
    end
  end

  test "handles empty repos list" do
    tool = GithubListReposTool.new
    setup_tool_mocks(tool)

    Github::App.stub :get_user_repos, { success: true, repos: [] } do
      tool.perform
      rendered = tool.instance_variable_get(:@rendered)
      assert rendered[:text]&.include?("No repositories found")
    end
  end
end

class GithubSearchReposToolTest < ActiveSupport::TestCase
  include MCPToolTestHelper

  setup do
    @repos = [ symbolize_keys_deep(load_github_fixture("repository")) ]
  end

  test "searches repositories on success" do
    tool = GithubSearchReposTool.new(query: "rails language:ruby", sort: "stars")
    setup_tool_mocks(tool)

    Github::App.stub :search_repos, { success: true, repos: @repos, total_count: 1 } do
      tool.perform
      assert_nil tool.instance_variable_get(:@error)
    end
  end

  test "validates query presence" do
    tool = GithubSearchReposTool.new(query: "")
    assert_not tool.valid?
  end

  test "handles no results" do
    tool = GithubSearchReposTool.new(query: "nonexistent-repo-xyz")
    setup_tool_mocks(tool)

    Github::App.stub :search_repos, { success: true, repos: [], total_count: 0 } do
      tool.perform
      rendered = tool.instance_variable_get(:@rendered)
      assert rendered[:text]&.include?("No repositories found")
    end
  end
end
