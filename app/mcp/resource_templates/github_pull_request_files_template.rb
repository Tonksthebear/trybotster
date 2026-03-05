# frozen_string_literal: true

class GithubPullRequestFilesTemplate < ApplicationMCPResTemplate
  uri_template "github://repos/{owner}/{repo}/pulls/{number}/files"
  template_name "github_pull_request_files"
  description "Get the files changed in a pull request, including the full diff patch for each file."
  mime_type "application/json"

  parameter :owner, description: "Repository owner", required: true
  parameter :repo, description: "Repository name", required: true
  parameter :number, description: "Pull request number", required: true

  def resolve
    full_repo = "#{owner}/#{repo}"
    installation_id = ::Github::App.installation_id_for_repo(full_repo)
    raise "GitHub App is not installed on #{full_repo}" unless installation_id

    client = ::Github::App.installation_client(installation_id)
    files = client.pull_request_files(full_repo, number.to_i)

    ActionMCP::Content::Resource.new(
      "github://repos/#{owner}/#{repo}/pulls/#{number}/files",
      "application/json",
      text: files.map(&:to_h).to_json
    )
  rescue Octokit::Error => e
    raise "Failed to fetch pull request files: #{e.message}"
  end
end
