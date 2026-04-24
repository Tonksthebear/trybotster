-- Embedded default web layout (wire protocol v2).
--
-- Entry point for `web_layout.render(surface, state)`. Produces a `UiNodeV1`
-- tree that authors composite primitives (`ui.session_list{}`,
-- `ui.new_session_button{}`, etc.) and lets each renderer expand them by
-- reading from its own entity store.
--
-- Pre-v2 the workspace surface inlined the entire workspace-grouped tree
-- here (~400 lines, hosted-preview indicators, accessory subtitles, nav
-- entries). Under v2 the renderers own that complexity:
--
--   * Web:  app/frontend/components/composites/SessionList.tsx
--   * TUI:  cli/src/tui/ui_contract_adapter/primitive.rs::render_session_list
--
-- See `cli/src/ui_contract/README.md` for the composite spec.

local M = {}

-- Density mapping from the surface variant ("workspace_sidebar" /
-- "workspace_panel") to the cross-client `UiSurfaceDensity` token.
local function density_for(state)
    if type(state) ~= "table" then return "panel" end
    return state.surface == "sidebar" and "sidebar" or "panel"
end

function M.workspace_surface(state)
    local density = density_for(state)
    local is_sidebar = density == "sidebar"

    return ui.stack{
        direction = "vertical",
        gap = is_sidebar and "0" or "2",
        children = {
            ui.session_list{
                density = density,
                grouping = "workspace",
                show_nav_entries = is_sidebar,
            },
            ui.new_session_button{
                action = ui.action("botster.session.create.request"),
            },
        },
    }
end

return M
