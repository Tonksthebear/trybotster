-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/github_integration.lua
-- @scope device
-- @version 1.1.0

local repo = require("project_pipelines.repo")
local engine = require("project_pipelines.engine")
local util = require("project_pipelines.util")

local M = {}

local function normalize_provider(provider)
    if util.is_blank(provider) then
        return "github"
    end
    return tostring(provider)
end

local function merge_summary(event, link)
    local pieces = {
        tostring(link.provider or event.provider or "github"),
        tostring(link.repo or event.repo),
        "#" .. tostring(link.pr_number or event.pr_number),
        "merged",
    }
    if not util.is_blank(event.merge_commit or link.merge_commit) then
        pieces[#pieces + 1] = "at " .. tostring(event.merge_commit or link.merge_commit)
    end
    return table.concat(pieces, " ")
end

function M.handle_pr_merged(event)
    event = event or {}
    if util.is_blank(event.repo) or util.is_blank(event.pr_number) then
        return { ok = false, reason = "missing_repo_or_pr_number" }
    end

    local link = repo.find_pr_link{
        provider = normalize_provider(event.provider),
        repo = event.repo,
        pr_number = event.pr_number,
    }
    if not link then
        return { ok = false, reason = "pr_not_linked" }
    end

    link = repo.mark_pr_link_merged(link.id, {
        pr_url = event.pr_url,
        head_branch = event.head_branch,
        base_branch = event.base_branch,
        merge_commit = event.merge_commit,
        merged_at = event.merged_at,
    })

    local ticket = repo.get_ticket(link.ticket_id)
    if ticket and ticket.status ~= "closed" then
        ticket = engine.close_ticket(link.ticket_id, {
            merge_confirmed = true,
            provider = link.provider,
            repo = link.repo,
            pr_number = link.pr_number,
            pr_url = event.pr_url or link.pr_url,
            merge_commit = event.merge_commit or link.merge_commit,
            merge_summary = merge_summary(event, link),
            source_event = "pr_merged",
        })
    end

    return { ok = true, pr_link = link, ticket = ticket }
end

return M
