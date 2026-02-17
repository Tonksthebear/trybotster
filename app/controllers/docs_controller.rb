class DocsController < ApplicationController
  layout "application"

  SECTIONS = [
    { title: "Getting Started", slug: "getting-started", pages: [
      { title: "Installation", slug: "installation" },
      { title: "Quick Start", slug: "quick-start" }
    ] },
    { title: "Configuration", slug: "configuration", pages: [
      { title: "Config File", slug: "config-file" },
      { title: "Environment Variables", slug: "environment-variables" },
      { title: "Profiles", slug: "profiles" }
    ] },
    { title: "Project Setup", slug: "project-setup", pages: [
      { title: "Directory Structure", slug: "directory-structure" },
      { title: "Sessions", slug: "sessions" },
      { title: "Workspace Files", slug: "workspace-files" }
    ] },
    { title: "Templates", slug: "templates", pages: [
      { title: "Using Templates", slug: "using-templates" }
    ] },
    { title: "TUI", slug: "tui", pages: [
      { title: "Keybindings", slug: "keybindings" },
      { title: "Navigation", slug: "navigation" }
    ] },
    { title: "Lua", slug: "lua", pages: [
      { title: "Plugins", slug: "plugins" },
      { title: "Custom Commands", slug: "custom-commands" }
    ] }
  ].freeze

  def show
    @path = params[:path] || first_page_path
    @sections = SECTIONS
    @current_section = @path.split("/").first

    # Path "getting-started/installation" → partial "docs/getting-started/installation"
    # Split into prefix path and partial name for lookup_context
    parts = @path.split("/")
    partial_name = parts.pop
    prefix = ([ "docs" ] + parts).join("/")

    if lookup_context.exists?(partial_name, [ prefix ], true)
      @page_partial = "docs/#{@path}"
    else
      redirect_to doc_path(path: first_page_path)
    end
  end

  private

  def first_page_path
    s = SECTIONS.first
    "#{s[:slug]}/#{s[:pages].first[:slug]}"
  end
end
