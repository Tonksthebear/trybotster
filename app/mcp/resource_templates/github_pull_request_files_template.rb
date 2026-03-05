# frozen_string_literal: true

class GithubPullRequestFilesTemplate < ApplicationMCPResTemplate
  uri_template "github://repos/{owner}/{repo}/pulls/{number}/files"
  template_name "github_pull_request_files"
  description <<~DESC.strip
    Get the files changed in a pull request, including the full diff patch for each file.

    The patch for each file is annotated with right-side line numbers — these are the
    exact values to use as `line` when submitting inline review comments via
    github_create_pull_request_review. Lines prefixed with a number are valid targets;
    lines prefixed with spaces (deletions) are left-side only and cannot be commented on
    without specifying side: LEFT (not supported by the review tool).

    Use this resource as the first step before writing a review with inline comments.
  DESC
  mime_type "application/json"

  parameter :owner, description: "Repository owner", required: true
  parameter :repo, description: "Repository name", required: true
  parameter :number, description: "Pull request number", required: true

  def resolve
    full_repo = "#{owner}/#{repo}"
    pr_number = number.to_i

    installation_id = ::Github::App.installation_id_for_repo(full_repo)
    unless installation_id
      return { error: "GitHub App is not installed on #{full_repo}" }.to_json
    end

    client = ::Github::App.installation_client(installation_id)
    files = client.pull_request_files(full_repo, pr_number)

    files.map do |file|
      {
        filename: file[:filename],
        status: file[:status],
        additions: file[:additions],
        deletions: file[:deletions],
        changes: file[:changes],
        previous_filename: file[:previous_filename],
        patch: file[:patch].present? ? annotate_patch(file[:patch]) : nil
      }.compact
    end.to_json
  rescue Octokit::Error => e
    { error: "Failed to fetch pull request files: #{e.message}" }.to_json
  rescue => e
    { error: "Error fetching pull request files: #{e.message}" }.to_json
  end

  private

  def owner
    arguments["owner"]
  end

  def repo
    arguments["repo"]
  end

  def number
    arguments["number"]
  end

  # Annotates a unified diff patch with right-side (new file) line numbers.
  # Numbered lines are valid targets for inline review comments (`line` parameter).
  # Deleted lines (starting with -) have no right-side number and are shown with blank prefix.
  def annotate_patch(patch)
    new_line = 0
    patch.each_line.map do |raw|
      line = raw.chomp
      if line.start_with?("@@")
        new_line = line[/\+(\d+)/, 1].to_i
        line
      elsif line.start_with?("-")
        "       #{line}"
      elsif line.start_with?("+")
        annotated = "#{new_line.to_s.rjust(6)} #{line}"
        new_line += 1
        annotated
      else
        annotated = "#{new_line.to_s.rjust(6)} #{line}"
        new_line += 1
        annotated
      end
    end.join("\n")
  end
end
