# frozen_string_literal: true

require "test_helper"
require "ostruct"

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

    assert_equal "Botster codex (realignment, sess-1234567)", tool.detect_client_type
  end

  test "ignores MCP host brand metadata" do
    tool = GithubListReposTool.new
    tool.define_singleton_method(:execution_context) do
      {
        session: OpenStruct.new(client_info: { "name" => "Known Host", "version" => "1.0" }),
        request: {
          params: {
            _meta: {
              "host.conversationId" => "abc"
            }
          }
        }
      }
    end

    assert_equal "Botster MCP session", tool.detect_client_type
  end
end
