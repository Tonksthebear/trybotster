# frozen_string_literal: true

require "test_helper"
class ApplicationMCPToolTest < ActiveSupport::TestCase
  test "attributes tools to Botster agent metadata" do
    tool = GithubListReposTool.new
    tool.define_singleton_method(:execution_context) do
      {
        request: {
          params: {
            _meta: {
              "botster" => {
                "agent_name" => "codex",
                "branch_name" => "realignment",
                "session_uuid" => "sess-1234567890abcdef"
              }
            }
          }
        }
      }
    end

    assert_equal "Botster codex (realignment, sess-1234567)", tool.botster_session_attribution
  end

  test "reads string-keyed request metadata" do
    tool = GithubListReposTool.new
    tool.define_singleton_method(:execution_context) do
      {
        "request" => {
          "params" => {
            "_meta" => {
              "botster" => {
                "session_name" => "reviewer",
                "branch_name" => "main"
              }
            }
          }
        }
      }
    end

    assert_equal "Botster reviewer (main)", tool.botster_session_attribution
  end

  test "ignores MCP host metadata" do
    tool = GithubListReposTool.new
    tool.define_singleton_method(:execution_context) do
      {
        request: {
          params: {
            _meta: {
              "host.conversationId" => "abc"
            }
          }
        }
      }
    end

    assert_equal "Botster MCP session", tool.botster_session_attribution
  end
end
