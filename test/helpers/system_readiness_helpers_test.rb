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
      @calls << [:assert_selector, selector, opts]
      true
    end

    def assert_no_selector(selector, **opts)
      @calls << [:assert_no_selector, selector, opts]
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

  test "wait_for_surface_ready asserts present then absent-loading" do
    @fake.wait_for_surface_ready("workspace_panel")

    assert_equal [
      [ :assert_selector, "[data-surface-ready='workspace_panel']", { wait: 15 } ],
      [ :assert_no_selector,
       "[data-surface-ready='workspace_panel'][data-surface-ready-state='loading']",
       { wait: 1 } ]
    ], @fake.calls
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
