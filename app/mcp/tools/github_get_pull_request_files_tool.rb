# frozen_string_literal: true

class GithubGetPullRequestFilesTool < ApplicationMCPTool
  tool_name "github_get_pull_request_files"
  description <<~DESC
    Get the files changed in a pull request, including the full diff patch for each file.

    The patch for each file is annotated with right-side line numbers — these are the
    exact values to use as `line` when submitting inline review comments via
    github_create_pull_request_review. Lines prefixed with a number are valid targets;
    lines prefixed with spaces (deletions) are left-side only and cannot be commented on
    without specifying side: LEFT (not supported by the review tool).

    Use this tool as the first step before writing a review with inline comments.
  DESC

  property :repo, type: "string", description: "Repository in 'owner/repo' format (e.g., 'octocat/Hello-World')", required: true
  property :pr_number, type: "integer", description: "Pull request number", required: true

  validates :repo, format: { with: /\A[\w\-\.]+\/[\w\-\.]+\z/, message: "must be in 'owner/repo' format" }
  validates :pr_number, numericality: { only_integer: true, greater_than: 0 }

  def perform
    installation_id = ::Github::App.installation_id_for_repo(repo)
    unless installation_id
      report_error("GitHub App is not installed on #{repo}")
      return
    end

    client = ::Github::App.installation_client(installation_id)

    client_info = detect_client_type
    render(text: "Fetching changed files for #{repo}##{pr_number} (via #{client_info})...")

    files = client.pull_request_files(repo, pr_number.to_i)

    if files.empty?
      render(text: "No changed files found in #{repo}##{pr_number}")
      return
    end

    output = [
      "📁 Changed files in #{repo}##{pr_number} (#{files.count} file#{"s" unless files.count == 1})",
      "=" * 80,
      ""
    ]

    files.each_with_index do |file, index|
      status_icon = case file[:status]
      when "added"    then "➕"
      when "removed"  then "➖"
      when "modified" then "✏️ "
      when "renamed"  then "🔄"
      else                 "📄"
      end

      header = "#{index + 1}. #{status_icon} #{file[:filename]}"
      header += "  (was: #{file[:previous_filename]})" if file[:previous_filename].present?
      output << header
      output << "   +#{file[:additions]} −#{file[:deletions]}  (#{file[:changes]} change#{"s" unless file[:changes] == 1})"

      if file[:patch].present?
        output << ""
        output << "   Diff (line numbers on left = valid `line` values for inline review comments):"
        output << annotate_patch(file[:patch]).gsub(/^/, "   ")
      end

      output << ""
    end

    total_adds = files.sum { |f| f[:additions] }
    total_dels = files.sum { |f| f[:deletions] }
    output << "Total: +#{total_adds} −#{total_dels} across #{files.count} file#{"s" unless files.count == 1}"

    render(text: output.join("\n"))
  rescue Octokit::Error => e
    report_error("Failed to fetch pull request files: #{e.message}")
  rescue => e
    report_error("Error fetching pull request files: #{e.message}")
  end

  private

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
