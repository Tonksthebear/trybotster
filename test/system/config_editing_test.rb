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

  test "device config tree loads current editing controls" do
    @cli = start_cli(@hub)

    sign_in_and_connect
    click_settings_link
    switch_to_device_scope

    assert_selector "[data-hub-settings-target='treePanel'][data-view='tree']", wait: 30
    assert_selector "[data-hub-settings-target='treeContainer']", wait: 10
    assert_button "+ Add Agent"
    assert_button "+ Add Accessory"
  end

  test "selecting a file loads its content in editor" do
    @cli = start_cli(@hub)

    sign_in_and_connect
    click_settings_link
    switch_to_device_scope
    wait_for_settings_ready("device")

    add_agent_named("alpha")
    select_agent_file("alpha")

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
    assert_selector "[data-hub-settings-target='editor']"
  end

  test "editing and saving a config file" do
    @cli = start_cli(@hub)

    sign_in_and_connect
    click_settings_link
    switch_to_device_scope

    add_agent_named("editor")
    select_agent_file("editor")

    # Wait for editor to load the file content
    assert_selector "[data-hub-settings-target='editorPanel'][data-editor='editing']", wait: 15

    # Save button should start disabled (no changes)
    save_btn = find("[data-hub-settings-target='saveBtn']", wait: 10)
    assert save_btn.disabled?, "Save button should be disabled before edits"

    # Modify editor content
    editor = find("[data-hub-settings-target='editor']")
    editor.set("#!/bin/bash\n# Modified by system test\necho 'hello from config editing test'\n")

    # Trigger an input event so the editor change detection runs
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

  test "adding an agent creates an editable initialization file" do
    @cli = start_cli(@hub)

    sign_in_and_connect
    click_settings_link
    switch_to_device_scope

    add_agent_named("fresh")
    select_agent_file("fresh")

    assert_selector "[data-hub-settings-target='editorPanel'][data-editor='editing']", wait: 15

    editor = find("[data-hub-settings-target='editor']")
    assert editor.value.present?, "Newly initialized file should have default content"
  end

  # ========== Templates Tab ==========

  test "templates tab shows available templates" do
    @cli = start_cli(@hub)

    sign_in_and_connect
    click_settings_link

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
    click_settings_link

    # Switch to templates tab and wait for DataChannel installed-check to complete.
    # The hub-templates controller sets data-hub-templates-ready after #checkInstalled,
    # ensuring the connection is live before we try to install.
    find("[data-tab='templates']").click
    assert_selector "[data-hub-templates-ready]", wait: 15

    # Click the github plugin template card (plugins are tracked by template:list,
    # unlike non-plugin templates like user/init.lua which aren't detected)
    plugin_card = find("[data-hub-templates-target='card'][data-dest*='plugins/github']", wait: 10)
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

  # Navigate to settings via Turbo by clicking the Settings link on the hub page.
  # The link is enabled by requires-connection controller once DataChannel is up.
  def click_settings_link
    find("a[href='#{hub_settings_path(@hub)}']", match: :first, wait: 10).click
    assert_selector "[data-hub-settings-target='treePanel']", wait: 10
  end

  def switch_to_device_scope
    click_button "Device", wait: 10
    assert_selector "[data-hub-settings-target='treePanel']", wait: 10
  end

  def add_agent_named(name)
    click_button "+ Add Agent", wait: 10
    assert_text "Add Agent", wait: 10
    find("input[autocomplete='off']", wait: 10).set(name)
    click_button "Create"
    assert_selector "[data-hub-settings-target='treeContainer'] button[data-file-path='agents/#{name}/initialization']",
                    wait: 15
  end

  def select_agent_file(name)
    find("[data-hub-settings-target='treeContainer'] button[data-file-path='agents/#{name}/initialization']",
         wait: 10).click
  end
end
