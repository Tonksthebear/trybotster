# @template Botster Bugs
# @description Route Botster bug reports to a live Codex orchestrator that files Project Pipelines tickets
# @category plugins
# @dest plugins/botster-bugs/README.md
# @scope device
# @version 1.0.0

# Botster Bugs

Device plugin that exposes `file_botster_bug` to agents.

The plugin is intentionally stateless. It does not use `plugin.db` or maintain a
bug ledger. Reports are sent to a live Codex orchestrator in the `Botster Bugs`
workspace, or embedded in the initial prompt when the orchestrator is created.

The orchestrator owns durability by creating Project Pipelines tickets and
delegating implementation, review, and verification through the pipeline tools.
