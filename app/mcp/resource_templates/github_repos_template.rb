# frozen_string_literal: true

class GithubReposTemplate < ApplicationMCPResTemplate
  uri_template "github://repos"
  template_name "github_repos"
  description "List GitHub repositories accessible to the bot. Returns repository name, description, URL, language, stars, and last update."
  mime_type "application/json"

  def resolve
    repos = ::Github::App.list_installation_repos

    if repos.empty?
      return [].to_json
    end

    # Sort by updated_at descending by default
    repos.sort_by! { |r| r[:updated_at] || "" }
    repos.reverse!

    repos.first(30).map do |repo|
      {
        full_name: repo[:full_name],
        description: repo[:description],
        html_url: repo[:html_url],
        language: repo[:language],
        stargazers_count: repo[:stargazers_count],
        updated_at: repo[:updated_at]
      }
    end.to_json
  rescue => e
    { error: "Error fetching repositories: #{e.message}" }.to_json
  end
end
