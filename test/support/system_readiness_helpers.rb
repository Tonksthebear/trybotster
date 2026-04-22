# frozen_string_literal: true

# Capybara helpers that wait on CAUSAL preconditions instead of downstream
# leaf UI effects. Paired with DOM signals written by the React shell:
#
#   <html data-cli-status="unknown|handshaking|connected|offline">
#   <html data-hub-snapshot="pending|received">
#   [data-surface-ready="<surface>" data-surface-ready-state="loading|ready"]
#   [data-settings-ready="device|repo" data-settings-ready-state="tree|empty"]
#
# All signals are derived from real state (no timers). Source of each is
# documented in app/frontend/components/AppShell.jsx (RootReadinessSignals),
# app/frontend/components/UiTree.jsx, and
# app/frontend/components/settings/ConfigEditor.jsx.
module SystemReadinessHelpers
  # Wait until the browser can dispatch to the hub AND the hub has shipped
  # its first snapshots (route registry + primary surface tree). After this
  # returns, button clicks / asserts can rely on the hub being reachable
  # without layering their own timeouts on top of the causal path.
  def wait_for_hub_ready(timeout: 30)
    assert_selector "html[data-cli-status='connected']", wait: timeout
    assert_selector "html[data-hub-snapshot='received']", wait: timeout
  end

  # Wait until a specific surface's UiTree has received its first frame.
  # `name` matches the `targetSurface` identifier (e.g. "workspace_panel",
  # "workspace_sidebar", or a plugin-registered surface).
  #
  # The compound selector matches ONLY when the surface has flipped to ready
  # — there is no fixed sub-timeout budget on the loading→ready transition,
  # so a slow surface on a slow CI still resolves as long as the total wait
  # fits within `timeout`. The UiTree wrapper mounts in `loading` state on
  # first paint, which is why we cannot key on the surface-ready attribute
  # alone: its presence means "the surface mounted", not "its tree arrived".
  def wait_for_surface_ready(name, timeout: 15)
    assert_selector(
      "[data-surface-ready='#{name}'][data-surface-ready-state='ready']",
      wait: timeout
    )
  end

  # Wait until the Hub Settings config tab has finished scanning the given
  # scope ("device" or "repo"). Default `state: :any` accepts both "tree"
  # (files found) and "empty" (scan complete, nothing there) — narrow if
  # the test specifically needs one.
  def wait_for_settings_ready(scope, state: :any, timeout: 15)
    selector = "[data-settings-ready='#{scope}']"
    selector += "[data-settings-ready-state='#{state}']" unless state == :any
    assert_selector selector, wait: timeout
  end
end
