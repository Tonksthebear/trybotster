# frozen_string_literal: true

class GithubReposTemplate < ApplicationMCPResTemplate
  uri_template "github://repos"
  template_name "github_repos"
  description "List GitHub repositories accessible to the bot."
  mime_type "application/json"

  def resolve
    repos = ::Github::App.list_installation_repos

    ActionMCP::Content::Resource.new(
      "github://repos",
      "application/json",
      text: repos.map { |r|
        {
          full_name: r[:full_name],
          description: r[:description],
          html_url: r[:html_url],
          language: r[:language],
          stargazers_count: r[:stargazers_count],
          updated_at: r[:updated_at]
        }
      }.to_json
    )
  rescue => e
    raise "Error fetching repositories: #{e.message}"
  end
end
