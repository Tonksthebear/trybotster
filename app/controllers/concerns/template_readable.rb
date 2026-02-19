# frozen_string_literal: true

module TemplateReadable
  extend ActiveSupport::Concern

  private

  # Read a template file and extract its @tag metadata.
  # Returns { name:, description:, dest:, scope:, version:, content: } or nil.
  def read_template(relative_path)
    path = Rails.root.join("app/templates", relative_path)
    return nil unless File.exist?(path)

    content = File.read(path)
    meta = extract_template_metadata(content)
    {
      name: meta[:template],
      description: meta[:description],
      dest: meta[:dest],
      scope: meta[:scope],
      version: meta[:version] || "1.0.0",
      content: content
    }
  end

  # Extract @tag metadata from comment headers.
  # Supports both Lua (`-- @tag value`) and shell (`# @tag value`) comments.
  # Stops at the first non-comment line (ignoring shebangs and blank lines).
  def extract_template_metadata(content)
    metadata = {}
    content.each_line do |line|
      next if line.start_with?("#!") # skip shebang
      next if line.strip.empty?
      break unless line.start_with?("--") || line.start_with?("#")

      if (match = line.match(/^(?:--|#)\s*@(\w+)\s+(.+)/))
        metadata[match[1].to_sym] = match[2].strip
      end
    end
    metadata
  end
end
