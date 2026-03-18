-- Accessory class for non-AI PTY sessions.
--
-- Inherits from Session (lib/session.lua) which owns:
-- - Session UUID generation, PTY lifecycle, manifest sync
-- - Metadata store, environment building, broker integration
-- - Session registry and lookup functions
--
-- Accessory is minimal — a plain PTY session without AI autonomy.
-- Examples: rails server, log tailing, port forwarding.
--
-- Single-PTY model: Accessory = 1 PTY without AI autonomy.
-- Session UUID is the primary key for everything.
--
-- This module is hot-reloadable; state is persisted via hub.state.
-- Uses state.class() for persistent metatable -- existing instances
-- automatically see new/changed methods after hot-reload.

local state = require("hub.state")
local Session = require("lib.session")

local Accessory = state.class("Accessory")

-- Inherit from Session: Accessory instances and class-level lookups
-- fall through to Session when not found on Accessory.
setmetatable(Accessory, { __index = Session })

-- =============================================================================
-- Constructor
-- =============================================================================

--- Create a new Accessory and spawn its single PTY session.
--
-- Config table: see Session._init for shared fields.
--
-- @param config Table of accessory configuration
-- @return Accessory instance
function Accessory.new(config)
    config.session_type = config.session_type or "accessory"
    local self = setmetatable({}, Accessory)
    Session._init(self, config)
    return self
end

--- Recover an Accessory from a persisted manifest during broker recovery.
-- @param config Table with manifest fields + handle/broker_session_id/dims
-- @return Accessory instance (first-class, identical to Accessory.new())
function Accessory.from_recovery(config)
    config.session_type = config.session_type or "accessory"
    local self = setmetatable({}, Accessory)
    Session._init_recovered(self, config)
    return self
end

-- =============================================================================
-- Lifecycle Hooks for Hot-Reload
-- =============================================================================

function Accessory._before_reload()
    log.info("accessory.lua reloading")
end

function Accessory._after_reload()
    log.info(string.format("accessory.lua reloaded -- %d sessions preserved", Session.count()))
end

return Accessory
