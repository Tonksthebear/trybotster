# frozen_string_literal: true

module TemplateReadable
  extend ActiveSupport::Concern

  TEMPLATE_SOURCE_ROOT = Rails.root.join("app/templates")
  TEMPLATE_SOURCE_PATTERN = "**/*.{lua,sh,md}"

  private

  # Read a template file and extract its metadata from the template source root.
  # Returns { name:, description:, dest:, scope:, version:, content: } or nil.
  def read_template(relative_path)
    path = template_source_root.join(relative_path)
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

  # Parse the template source root into a grouped catalog.
  # Rails treats templates as filesystem data: metadata comes from comment
  # headers, file content is served as-is, and category grouping is metadata.
  def template_catalog
    template_source_files.filter_map { |path|
      content = File.read(path)
      meta = extract_template_metadata(content)
      next unless meta[:template] && meta[:category] && meta[:dest]

      relative = Pathname.new(path).relative_path_from(template_source_root).to_s
      ext = File.extname(relative)
      basename = relative.delete_suffix(ext).tr("/", "-")

      {
        slug: "#{meta[:category]}-#{basename}",
        name: meta[:template],
        description: meta[:description],
        category: meta[:category],
        dest: meta[:dest],
        scope: meta[:scope],
        version: meta[:version] || "1.0.0",
        content: content
      }
    }.group_by { |template| template[:category] }
  end

  def template_source_root
    TEMPLATE_SOURCE_ROOT
  end

  def template_source_files
    Dir.glob(template_source_root.join(TEMPLATE_SOURCE_PATTERN)).sort
  end

  # Extract @tag metadata from comment headers.
  # Supports Lua (`-- @tag value`), shell (`# @tag value`), and Markdown
  # HTML comments (`<!-- @tag value -->`).
  # Stops at the first non-comment line (ignoring shebangs and blank lines).
  def extract_template_metadata(content)
    metadata = {}
    content.each_line do |line|
      next if line.start_with?("#!") # skip shebang
      next if line.strip.empty?
      break unless line.start_with?("--") || line.start_with?("#") || line.start_with?("<!--")

      if (match = line.match(/^(?:--|#)\s*@(\w+)\s+(.+)/))
        metadata[match[1].to_sym] = match[2].strip
      elsif (match = line.match(/^<!--\s*@(\w+)\s+(.+?)\s*-->/))
        metadata[match[1].to_sym] = match[2].strip
      end
    end
    metadata
  end
end
