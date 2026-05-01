# Project Pipelines Verification

## 2026-05-01 Dependency And Merge Polish

Manual verification was run against the live `~/.botster-dev/plugins/project-pipelines` plugin after reloading the plugin and layout.

Scope:

- Ticket dependency CRUD.
- Dependency blocker enforcement before run start.
- Dependency cleanup when deleting tickets.
- Cycle rejection.
- Template catalog parity for Project Pipelines.

Results:

- Created throwaway tickets `ticket_1777675926_812500` and `ticket_1777675938_308597`.
- Added dependency `ticket_1777675926_812500 -> ticket_1777675938_308597`; `project_pipelines_list_ticket_dependencies` returned the dependency with `depends_on_status = "open"`.
- Attempted self-dependency `ticket_1777675926_812500 -> ticket_1777675926_812500`; rejected with `ticket cannot depend on itself`.
- Attempted cycle `ticket_1777675938_308597 -> ticket_1777675926_812500`; rejected with `ticket dependency would create a cycle`.
- Attempted `project_pipelines_start_run` on dependent ticket while dependency was open; rejected before pipeline lookup with `ticket dependencies must close before starting a run: Verification dependency B (open)`.
- Deleted dependency ticket `ticket_1777675938_308597`; dependency lists for `ticket_1777675926_812500` returned empty results, proving dependency rows were purged.
- Retried `project_pipelines_start_run` with a deliberately missing pipeline; error changed to `pipeline not found`, proving dependency enforcement was no longer blocking.
- Deleted all throwaway verification tickets, including earlier cleanup tickets `ticket_1777675710_491621` and `ticket_1777675721_427180`.

Static and catalog checks:

- `luac -p` passed for live and template Project Pipelines Lua files.
- `cli/./test.sh --integration -- project_pipelines_template_catalog_entry_is_a_multi_file_plugin` passed.

Code inspection:

- `project_pipelines_close_ticket` now accepts `merge_commit`, `pr_url`, and `merge_summary` through MCP, threads them through `engine.close_ticket`, and writes a `kind = "merge"` artifact before closing when `merge_confirmed = true`.
- Returning to an existing step agent sends both `Hub:post(... type = "task")` and `Hub:notify(...)` pointing the agent to `project_pipelines_current_context`.
