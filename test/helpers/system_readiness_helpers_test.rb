# frozen_string_literal: true

require "test_helper"

# Shape-only test for SystemReadinessHelpers. These helpers wrap Capybara's
# assert_selector / assert_no_selector with fixed selector strings — the
# real assertion is that the selectors match what the React shell emits
# (covered by the vitest suite `system-readiness-signals.test.jsx` and by
# the 3 migrated flaking system tests). This suite guards the wrapping
# layer: that each helper exists, takes the expected args, and issues the
# correct Capybara call shape with the correct selector.
class SystemReadinessHelpersTest < ActiveSupport::TestCase
  class FakeCapybara
    include SystemReadinessHelpers

    attr_reader :calls

    def initialize
      @calls = []
    end

    def assert_selector(selector, **opts)
      @calls << [ :assert_selector, selector, opts ]
      true
    end

    def assert_no_selector(selector, **opts)
      @calls << [ :assert_no_selector, selector, opts ]
      true
    end
  end

  setup do
    @fake = FakeCapybara.new
  end

  test "wait_for_hub_ready asserts cli-status=connected and hub-snapshot=received" do
    @fake.wait_for_hub_ready(timeout: 7)

    assert_equal 2, @fake.calls.size
    kind, selector, opts = @fake.calls[0]
    assert_equal :assert_selector, kind
    assert_equal "html[data-cli-status='connected']", selector
    assert_equal 7, opts[:wait]

    kind, selector, opts = @fake.calls[1]
    assert_equal :assert_selector, kind
    assert_equal "html[data-hub-snapshot='received']", selector
    assert_equal 7, opts[:wait]
  end

  test "wait_for_surface_ready waits on the compound ready selector in a single assertion" do
    @fake.wait_for_surface_ready("workspace_panel")

    assert_equal [
      [ :assert_selector,
       "[data-surface-ready='workspace_panel'][data-surface-ready-state='ready']",
       { wait: 15 } ]
    ], @fake.calls
  end

  test "wait_for_surface_ready respects an overridden timeout" do
    @fake.wait_for_surface_ready("kanban", timeout: 30)

    assert_equal [
      [ :assert_selector,
       "[data-surface-ready='kanban'][data-surface-ready-state='ready']",
       { wait: 30 } ]
    ], @fake.calls
  end

  # Regression: the prior two-assertion shape split the wait budget
  # (timeout for present, hidden 1s for loading-gone) and flaked under CI
  # slowness. This test pins the new shape — the helper issues exactly ONE
  # Capybara assertion keyed on the READY-STATE compound selector, passing
  # the full `timeout` through. No hidden sub-budget, no separate
  # "presence" check that would match a still-loading surface.
  test "wait_for_surface_ready hands the full timeout to a single ready-state assertion" do
    @fake.wait_for_surface_ready("workspace_panel", timeout: 12)

    assert_equal 1, @fake.calls.size,
      "helper must NOT split the budget across multiple assertions"
    kind, selector, opts = @fake.calls.first
    assert_equal :assert_selector, kind
    assert_includes selector, "data-surface-ready-state='ready'",
      "must wait on the READY state, not just presence of the surface-ready attribute"
    assert_equal 12, opts[:wait],
      "full timeout must be passed to Capybara; no fixed sub-budget"
  end

  test "wait_for_settings_ready with default :any asserts scope only" do
    @fake.wait_for_settings_ready("device")

    assert_equal [
      [ :assert_selector, "[data-settings-ready='device']", { wait: 15 } ]
    ], @fake.calls
  end

  test "wait_for_settings_ready narrows by state when given" do
    @fake.wait_for_settings_ready("repo", state: "tree", timeout: 9)

    assert_equal [
      [ :assert_selector,
       "[data-settings-ready='repo'][data-settings-ready-state='tree']",
       { wait: 9 } ]
    ], @fake.calls
  end
end
