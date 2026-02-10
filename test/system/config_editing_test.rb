# frozen_string_literal: true

require "application_system_test_case"
require_relative "../support/cli_test_helper"

class ConfigEditingTest < ApplicationSystemTestCase
  include CliTestHelper

  driven_by :selenium, using: :headless_chrome, screen_size: [ 1280, 900 ]

  setup do
    @user = users(:one)
    @hub = create_test_hub(user: @user)
  end

  teardown do
    stop_cli(@cli) if @cli
    @hub&.destroy
  end

  # ========== Config Tab ==========

  test "settings page loads with config tab active" do
    sign_in_as(@user)
    visit hub_settings_path(@hub)

    # Config tab button should have data-active attribute
    assert_selector "[data-tab='config'][data-active]", wait: 10

    # Config tab panel should be visible (no hidden class)
    assert_selector "[data-tab-panel='config']", wait: 10
    assert_no_selector "[data-tab-panel='config'].hidden", visible: :all

    # Templates tab panel should be hidden
    assert_selector "[data-tab-panel='templates'].hidden", visible: :all

    # Tree panel should exist in loading/disconnected state (no CLI running)
    tree_panel = find("[data-hub-settings-target='treePanel']", wait: 10)
    assert_includes %w[loading disconnected], tree_panel["data-view"],
      "Tree panel should be loading or disconnected without CLI"
  end

  test "config tree loads files from CLI after Initialize" do
    @cli = start_cli(@hub)

    sign_in_and_connect
    visit hub_settings_path(@hub)

    # Without pre-existing .botster/ dir, tree shows empty state
    assert_selector "[data-hub-settings-target='treePanel'][data-view='empty']", wait: 30

    # Click Initialize to create default .botster/ structure via DataChannel
    find("[data-action='hub-settings#initBotster']", wait: 10).click

    # Tree should transition to "tree" view with file entries
    assert_selector "[data-hub-settings-target='treePanel'][data-view='tree']", wait: 15
    assert_selector "[data-hub-settings-target='treeContainer'] button[data-file-path]",
                    minimum: 1, wait: 10
  end

  test "selecting a file loads its content in editor" do
    @cli = start_cli(@hub)

    sign_in_and_connect
    visit hub_settings_path(@hub)

    # Initialize config structure first
    initialize_config_via_ui

    # Click a file entry in the tree
    file_button = first("[data-hub-settings-target='treeContainer'] button[data-file-path]")
    assert file_button, "Should have at least one file entry in tree"
    file_button.click

    # Editor panel should transition to "editing" (file exists) or "creating" (file missing)
    assert_selector(
      "[data-hub-settings-target='editorPanel'][data-editor='editing'], " \
      "[data-hub-settings-target='editorPanel'][data-editor='creating']",
      wait: 15
    )

    # Editor title should display the file path
    editor_title = find("[data-hub-settings-target='editorTitle']", wait: 10)
    assert editor_title.text.present?, "Editor title should show file path"
    assert_not_equal "Select a file", editor_title.text

    # Textarea should be visible
    assert_selector "[data-hub-settings-target='editor']", wait: 10
  end

  test "editing and saving a config file" do
    @cli = start_cli(@hub)

    sign_in_and_connect
    visit hub_settings_path(@hub)

    # Initialize and find a file that exists (initialization script)
    initialize_config_via_ui

    # Select the initialization file (should be in the tree after init)
    init_button = find(
      "[data-hub-settings-target='treeContainer'] button[data-file-path$='/initialization']",
      wait: 10
    )
    init_button.click

    # Wait for editor to load the file content
    assert_selector "[data-hub-settings-target='editorPanel'][data-editor='editing']", wait: 15

    # Save button should start disabled (no changes)
    save_btn = find("[data-hub-settings-target='saveBtn']", wait: 10)
    assert save_btn.disabled?, "Save button should be disabled before edits"

    # Modify editor content
    editor = find("[data-hub-settings-target='editor']")
    editor.set("#!/bin/bash\n# Modified by system test\necho 'hello from config editing test'\n")

    # Trigger input event so Stimulus detects the change
    editor.send_keys(" ")
    editor.send_keys(:backspace)

    # Save button should now be enabled
    assert_not find("[data-hub-settings-target='saveBtn']").disabled?,
      "Save button should be enabled after editing"

    # Click save
    find("[data-hub-settings-target='saveBtn']").click

    # Should show "Saved" feedback
    assert_selector "[data-hub-settings-target='saveBtn']", text: "Saved", wait: 10

    # After save, button should become disabled (content matches saved version)
    assert_selector "[data-hub-settings-target='saveBtn'][disabled]", wait: 10
  end

  test "creating a new config file via Initialize" do
    @cli = start_cli(@hub)

    sign_in_and_connect
    visit hub_settings_path(@hub)

    # Tree should show empty state (no .botster/ directory exists)
    assert_selector "[data-hub-settings-target='treePanel'][data-view='empty']", wait: 30

    # Click Initialize to create the default .botster/ structure
    find("[data-action='hub-settings#initBotster']", wait: 10).click

    # Tree should now show "tree" view with file entries
    assert_selector "[data-hub-settings-target='treePanel'][data-view='tree']", wait: 15

    # Should have at least the initialization file entry
    assert_selector "[data-hub-settings-target='treeContainer'] button[data-file-path]",
                    minimum: 1, wait: 10

    # Select the newly created initialization file
    init_btn = find(
      "[data-hub-settings-target='treeContainer'] button[data-file-path$='/initialization']",
      wait: 10
    )
    init_btn.click

    # Editor should show the file content (editing state, not creating)
    assert_selector "[data-hub-settings-target='editorPanel'][data-editor='editing']", wait: 15

    editor = find("[data-hub-settings-target='editor']")
    assert editor.value.present?, "Newly initialized file should have default content"
  end

  # ========== Templates Tab ==========

  test "templates tab shows available templates" do
    sign_in_as(@user)
    visit hub_settings_path(@hub)

    # Switch to templates tab
    find("[data-tab='templates']").click

    # Templates tab button should be active, config tab deactivated
    assert_selector "[data-tab='templates'][data-active]", wait: 10
    assert_no_selector "[data-tab='config'][data-active]"

    # Templates panel should be visible, config panel hidden
    assert_no_selector "[data-tab-panel='templates'].hidden", visible: :all, wait: 10
    assert_selector "[data-tab-panel='config'].hidden", visible: :all

    # Template catalog should render with cards
    assert_selector "[data-hub-templates-target='catalog']", wait: 10
    assert_selector "[data-hub-templates-target='card']", minimum: 1

    # Each card should have a badge
    assert_selector "[data-hub-templates-target='badge']", minimum: 1
  end

  test "installing a template writes files to CLI" do
    @cli = start_cli(@hub)

    sign_in_and_connect
    visit hub_settings_path(@hub)

    # Switch to templates tab
    find("[data-tab='templates']").click
    assert_selector "[data-hub-templates-target='catalog']", wait: 10

    # Click a plugin template card (plugins are tracked by template:list,
    # unlike non-plugin templates like user/init.lua which aren't detected)
    plugin_card = find("[data-hub-templates-target='card'][data-dest*='plugins/']", wait: 10)
    plugin_card.click

    # Preview panel should appear with an install button
    install_btn = find("[data-hub-templates-target='installBtn']", wait: 10)
    slug = install_btn["data-slug"]

    # Click Install
    install_btn.click

    # Button should transition to "Uninstall" after successful install
    assert_selector "[data-hub-templates-target='installBtn'][data-slug='#{slug}']",
                    text: "Uninstall", wait: 15

    # Navigate back to catalog and verify badge
    find("[data-action='hub-templates#backToCatalog']", wait: 10).click

    badge = find("[data-hub-templates-target='badge'][data-badge-for='#{slug}']", wait: 10)
    assert_match(/installed/, badge.text.strip,
      "Badge should show 'installed' after template installation")
  end

  private

  def sign_in_as(user)
    visit "/test/sessions/new?user_id=#{user.id}"
  end

  # Visit the hub page using the connection URL (which carries the E2E key
  # bundle in the URL fragment). Wait for the WebRTC DataChannel to be fully
  # established. The Olm session is persisted in IndexedDB so subsequent
  # page navigations (e.g., to settings) can reuse it without the fragment.
  def sign_in_and_connect
    url = @cli.connection_url
    assert url.present?, "CLI should produce a connection URL"

    sign_in_as(@user)
    visit url

    # Wait for WebRTC DataChannel to be established (direct or relay)
    assert_selector(
      "[data-connection-status-target='connectionSection'][data-state='direct'], " \
      "[data-connection-status-target='connectionSection'][data-state='relay']",
      wait: 30
    )
  end

  # Click Initialize in the settings UI to create the default .botster/
  # config structure via DataChannel, then wait for the file tree to render.
  def initialize_config_via_ui
    assert_selector "[data-hub-settings-target='treePanel'][data-view='empty']", wait: 30
    find("[data-action='hub-settings#initBotster']", wait: 10).click
    assert_selector "[data-hub-settings-target='treePanel'][data-view='tree']", wait: 15
    assert_selector "[data-hub-settings-target='treeContainer'] button[data-file-path]",
                    minimum: 1, wait: 10
  end
end
