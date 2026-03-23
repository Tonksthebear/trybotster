-- Agent class for AI-driven PTY sessions.
--
-- Inherits from Session (lib/session.lua) which owns:
-- - Session UUID generation, PTY lifecycle, manifest sync
-- - Metadata store, environment building, broker integration
-- - Session registry and lookup functions
--
-- Agent adds:
-- - Inter-agent message inbox
-- - AI-specific lifecycle hooks on top of the shared Session PTY model
--
-- Single-PTY model: Agent = 1 PTY with AI autonomy.
-- Session UUID is the primary key for everything.
--
-- This module is hot-reloadable; state is persisted via hub.state.
-- Uses state.class() for persistent metatable -- existing instances
-- automatically see new/changed methods after hot-reload.

local state = require("hub.state")
local Session = require("lib.session")

local Agent = state.class("Agent")

-- Inherit from Session: Agent instances and class-level lookups
-- fall through to Session when not found on Agent.
setmetatable(Agent, { __index = Session })

-- =============================================================================
-- Constructor
-- =============================================================================

--- Create a new Agent and spawn its single PTY session.
--
-- Config table: see Session._init for shared fields. Agent adds:
--   prompt          string   (optional)  task description for AI
--   metadata        table    (optional)  plugin key-value store
--
-- @param config Table of agent configuration
-- @return Agent instance
function Agent.new(config)
    config.session_type = config.session_type or "agent"
    local self = setmetatable({}, Agent)
    Session._init(self, config)
    self._inbox = {}  -- inter-agent message inbox: array of envelope tables
    return self
end

--- Recover an Agent from a persisted manifest during broker recovery.
-- @param config Table with manifest fields + handle/broker_session_id/dims
-- @return Agent instance (first-class, identical to Agent.new())
function Agent.from_recovery(config)
    config.session_type = config.session_type or "agent"
    local self = setmetatable({}, Agent)
    Session._init_recovered(self, config)
    self._inbox = {}
    return self
end

-- =============================================================================
-- Agent-Specific Methods
-- =============================================================================

--- Drain an agent's inbox, discarding expired messages.
-- @param session_uuid string Session UUID
-- @return array of envelope tables (may be empty), or nil if agent not found
function Agent.receive_messages(session_uuid)
    local agent = Agent.get(session_uuid)
    if not agent then return nil end
    -- Only agents have inboxes; accessories don't receive messages
    if agent.session_type ~= "agent" then return nil end

    local now = os.time()
    local valid = {}
    for _, envelope in ipairs(agent._inbox or {}) do
        if not envelope.expires_at or envelope.expires_at >= now then
            valid[#valid + 1] = envelope
        end
    end

    agent._inbox = {}
    return valid
end

-- =============================================================================
-- Lifecycle Hooks for Hot-Reload
-- =============================================================================

function Agent._before_reload()
    log.info("agent.lua reloading")
end

function Agent._after_reload()
    log.info(string.format("agent.lua reloaded -- %d sessions preserved", Session.count()))
end

return Agent
