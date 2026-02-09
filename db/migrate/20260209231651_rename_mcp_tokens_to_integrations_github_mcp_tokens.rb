class RenameMCPTokensToIntegrationsGithubMCPTokens < ActiveRecord::Migration[8.1]
  def change
    rename_table :mcp_tokens, :integrations_github_mcp_tokens
  end
end
