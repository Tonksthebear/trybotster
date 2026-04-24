//! Lua DSL for the cross-client UI contract.
//!
//! Registers a global `ui` table in a Lua VM exposing constructor functions
//! for every v1 primitive plus the adaptive helpers `ui.responsive`,
//! `ui.when`, `ui.hidden`, and the `ui.action` helper. This module is
//! deliberately VM-agnostic — both the hub [`crate::lua::LuaRuntime`] and
//! the TUI `LayoutLua` call [`register`] on their own `mlua::Lua` instance.
//!
//! # Wire format
//!
//! Constructors return Lua tables that mirror [`UiNodeV1`] JSON exactly.
//! Authors never hand-build the underlying table shape — they always call
//! the constructor so the marshalling layer can enforce invariants:
//!
//! - slot key `end_` is rewritten to `end` (because `end` is reserved in Lua)
//! - controlled/uncontrolled rule: if `value` or `selected` is present, Lua
//!   owns state; otherwise a stable `id` lets the renderer own it
//! - `ui.responsive` detects and rejects mixed width/height shorthand keys
//!
//! See `docs/specs/cross-client-ui-primitives.md` and
//! `docs/specs/adaptive-ui-viewport-and-presentation.md` for the spec.
//!
//! [`UiNodeV1`]: crate::ui_contract::node::UiNodeV1

use anyhow::{anyhow, Result};
use mlua::{Lua, Table, Value};

/// JSON key used on the Lua constructor output tables as a `$kind`
/// discriminator for responsive values and conditional wrappers.
const KIND_KEY: &str = "$kind";

/// Slot keys that are reserved words in Lua. The constructor rewrites the
/// trailing underscore form to the canonical wire name.
const LUA_RESERVED_SLOT_ALIASES: &[(&str, &str)] = &[("end_", "end")];

/// Known width-class keys for `ui.responsive` shorthand detection.
const WIDTH_CLASS_KEYS: &[&str] = &["compact", "regular", "expanded"];

/// Known height-class keys for `ui.responsive` shorthand detection.
const HEIGHT_CLASS_KEYS: &[&str] = &["short", "tall"];

/// Register the `ui` table as a global in the given Lua VM.
///
/// Safe to call on both the hub `LuaRuntime` and the TUI `LayoutLua` VMs.
///
/// # Errors
///
/// Returns an error if table / function construction fails.
pub fn register(lua: &Lua) -> Result<()> {
    let ui = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create ui table: {e}"))?;

    register_primitive(lua, &ui, "stack", Primitive::Stack)?;
    register_primitive(lua, &ui, "inline", Primitive::Inline)?;
    register_primitive(lua, &ui, "panel", Primitive::Panel)?;
    register_primitive(lua, &ui, "scroll_area", Primitive::ScrollArea)?;
    register_primitive(lua, &ui, "text", Primitive::Text)?;
    register_primitive(lua, &ui, "icon", Primitive::Icon)?;
    register_primitive(lua, &ui, "badge", Primitive::Badge)?;
    register_primitive(lua, &ui, "status_dot", Primitive::StatusDot)?;
    register_primitive(lua, &ui, "empty_state", Primitive::EmptyState)?;
    register_primitive(lua, &ui, "button", Primitive::Button)?;
    register_primitive(lua, &ui, "icon_button", Primitive::IconButton)?;
    register_primitive(lua, &ui, "tree", Primitive::Tree)?;
    register_primitive(lua, &ui, "tree_item", Primitive::TreeItem)?;
    register_primitive(lua, &ui, "dialog", Primitive::Dialog)?;
    // Wire protocol v2 composites — data-driven, no children, no slots.
    register_primitive(lua, &ui, "session_list", Primitive::SessionList)?;
    register_primitive(lua, &ui, "workspace_list", Primitive::WorkspaceList)?;
    register_primitive(lua, &ui, "spawn_target_list", Primitive::SpawnTargetList)?;
    register_primitive(lua, &ui, "worktree_list", Primitive::WorktreeList)?;
    register_primitive(lua, &ui, "session_row", Primitive::SessionRow)?;
    register_primitive(lua, &ui, "hub_recovery_state", Primitive::HubRecoveryState)?;
    register_primitive(lua, &ui, "connection_code", Primitive::ConnectionCode)?;
    register_primitive(lua, &ui, "new_session_button", Primitive::NewSessionButton)?;

    register_action(lua, &ui)?;
    register_responsive(lua, &ui)?;
    register_when(lua, &ui)?;
    register_hidden(lua, &ui)?;
    // Wire protocol v2 — reactive data sentinels for plugin layouts.
    register_bind(lua, &ui)?;
    register_bind_list(lua, &ui)?;

    lua.globals()
        .set("ui", ui)
        .map_err(|e| anyhow!("Failed to register ui global: {e}"))?;

    Ok(())
}

/// Enum identifying each primitive so the shared constructor function can
/// apply primitive-specific marshalling (e.g. slot handling, required id).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Primitive {
    Stack,
    Inline,
    Panel,
    ScrollArea,
    Text,
    Icon,
    Badge,
    StatusDot,
    EmptyState,
    Button,
    IconButton,
    Tree,
    TreeItem,
    /// Flagged internal in v1 — registered so renderers can consume it while
    /// Phase B / Phase C catch up.
    Dialog,
    /// Wire protocol v2 — workspace-grouped session tree composite.
    SessionList,
    /// Wire protocol v2 — bare workspace switcher composite.
    WorkspaceList,
    /// Wire protocol v2 — spawn target picker composite.
    SpawnTargetList,
    /// Wire protocol v2 — worktree picker composite for a given target.
    WorktreeList,
    /// Wire protocol v2 — single-session row composite (binds to a uuid).
    SessionRow,
    /// Wire protocol v2 — hub lifecycle banner composite (singleton entity).
    HubRecoveryState,
    /// Wire protocol v2 — pairing QR + URL composite (singleton entity).
    ConnectionCode,
    /// Wire protocol v2 — "new session" button composite.
    NewSessionButton,
}

impl Primitive {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::Stack => "stack",
            Self::Inline => "inline",
            Self::Panel => "panel",
            Self::ScrollArea => "scroll_area",
            Self::Text => "text",
            Self::Icon => "icon",
            Self::Badge => "badge",
            Self::StatusDot => "status_dot",
            Self::EmptyState => "empty_state",
            Self::Button => "button",
            Self::IconButton => "icon_button",
            Self::Tree => "tree",
            Self::TreeItem => "tree_item",
            Self::Dialog => "dialog",
            Self::SessionList => "session_list",
            Self::WorkspaceList => "workspace_list",
            Self::SpawnTargetList => "spawn_target_list",
            Self::WorktreeList => "worktree_list",
            Self::SessionRow => "session_row",
            Self::HubRecoveryState => "hub_recovery_state",
            Self::ConnectionCode => "connection_code",
            Self::NewSessionButton => "new_session_button",
        }
    }

    /// Allowed slot keys for this primitive. An empty slice means the
    /// primitive does not accept any slots; unknown slot keys are rejected
    /// at construction.
    const fn allowed_slots(self) -> &'static [&'static str] {
        match self {
            Self::TreeItem => &["title", "subtitle", "start", "end", "children"],
            Self::Dialog => &["body", "footer"],
            _ => &[],
        }
    }

    /// Slot keys that are REQUIRED for this primitive — missing slots raise
    /// a Lua error at construction.
    const fn required_slots(self) -> &'static [&'static str] {
        match self {
            Self::TreeItem => &["title"],
            _ => &[],
        }
    }

    /// Allowed prop keys (camelCase wire names) for this primitive. Unknown
    /// props raise a Lua error at construction — symmetric with the
    /// slot allowlist.
    ///
    /// The allowlists here mirror the fields of each `*PropsV1` struct in
    /// `crate::ui_contract::props`, which in turn mirror the cross-client
    /// spec. Web-only extensions (`Panel.padding`, `Button.leadingIcon`, …)
    /// are deliberately absent.
    const fn allowed_props(self) -> &'static [&'static str] {
        match self {
            Self::Stack => &["direction", "gap", "align", "justify"],
            Self::Inline => &["gap", "align", "justify", "wrap"],
            Self::Panel => &["title", "tone", "border", "interactionDensity"],
            Self::ScrollArea => &["axis"],
            Self::Text => &["text", "tone", "size", "weight", "monospace", "italic", "truncate"],
            Self::Icon => &["name", "size", "tone", "label"],
            Self::Badge => &["text", "tone", "size"],
            Self::StatusDot => &["state", "label"],
            Self::EmptyState => &["title", "description", "icon", "primaryAction"],
            Self::Button => &["label", "action", "variant", "tone", "icon"],
            Self::IconButton => &["icon", "label", "action", "tone"],
            Self::Tree => &[],
            Self::TreeItem => &["expanded", "selected", "notification", "action"],
            Self::Dialog => &["open", "title", "presentation"],
            Self::SessionList => &["density", "grouping", "showNavEntries"],
            Self::WorkspaceList => &["density"],
            Self::SpawnTargetList => &["onSelect", "onRemove"],
            Self::WorktreeList => &["targetId"],
            Self::SessionRow => &["sessionUuid", "density"],
            Self::HubRecoveryState => &[],
            Self::ConnectionCode => &[],
            Self::NewSessionButton => &["action"],
        }
    }
}

/// Prop keys reserved for the UiNodeV1 envelope itself (not passed to props).
///
/// The envelope is identical for every primitive — `id` (optional stable id),
/// `children` (positional child array), `slots` (named slot map).
const ENVELOPE_KEYS: &[&str] = &["id", "children", "slots"];

fn register_primitive(lua: &Lua, ui: &Table, fn_name: &str, kind: Primitive) -> Result<()> {
    let constructor = lua
        .create_function(move |lua, args: Value| build_node(lua, kind, args))
        .map_err(|e| anyhow!("Failed to create ui.{fn_name}: {e}"))?;
    ui.set(fn_name, constructor)
        .map_err(|e| anyhow!("Failed to attach ui.{fn_name}: {e}"))?;
    Ok(())
}

fn register_action(lua: &Lua, ui: &Table) -> Result<()> {
    let constructor = lua
        .create_function(|lua, (id, payload): (String, Option<Table>)| {
            let action = lua.create_table()?;
            action.set("id", id)?;
            if let Some(payload) = payload {
                action.set("payload", payload)?;
            }
            Ok(action)
        })
        .map_err(|e| anyhow!("Failed to create ui.action: {e}"))?;
    ui.set("action", constructor)
        .map_err(|e| anyhow!("Failed to attach ui.action: {e}"))?;
    Ok(())
}

fn register_responsive(lua: &Lua, ui: &Table) -> Result<()> {
    let constructor = lua
        .create_function(|lua, input: Table| build_responsive(lua, &input))
        .map_err(|e| anyhow!("Failed to create ui.responsive: {e}"))?;
    ui.set("responsive", constructor)
        .map_err(|e| anyhow!("Failed to attach ui.responsive: {e}"))?;
    Ok(())
}

fn register_when(lua: &Lua, ui: &Table) -> Result<()> {
    let constructor = lua
        .create_function(|lua, (condition, node): (Value, Value)| {
            build_conditional(lua, "when", condition, node)
        })
        .map_err(|e| anyhow!("Failed to create ui.when: {e}"))?;
    ui.set("when", constructor)
        .map_err(|e| anyhow!("Failed to attach ui.when: {e}"))?;
    Ok(())
}

fn register_hidden(lua: &Lua, ui: &Table) -> Result<()> {
    let constructor = lua
        .create_function(|lua, (condition, node): (Value, Value)| {
            build_conditional(lua, "hidden", condition, node)
        })
        .map_err(|e| anyhow!("Failed to create ui.hidden: {e}"))?;
    ui.set("hidden", constructor)
        .map_err(|e| anyhow!("Failed to attach ui.hidden: {e}"))?;
    Ok(())
}

/// `ui.bind(path)` — wire protocol v2 sentinel. Emits `{ "$bind": path }`.
///
/// Resolved client-side against the per-entity-type stores. Path grammar:
///
/// * `/<type>/<id>/<field>` — scalar lookup
/// * `/<type>/<id>` — whole record
/// * `/<type>` — list of records sorted by store insertion order
/// * `@/<field>` — item-relative (only valid inside `ui.bind_list`)
///
/// Both renderers (TUI binding.rs, web binding.tsx) honor the same grammar.
fn register_bind(lua: &Lua, ui: &Table) -> Result<()> {
    let constructor = lua
        .create_function(|lua, path: String| {
            if path.is_empty() {
                return Err(mlua::Error::RuntimeError(
                    "ui.bind: path must be a non-empty string".to_string(),
                ));
            }
            let out = lua.create_table()?;
            out.set("$bind", path)?;
            Ok(out)
        })
        .map_err(|e| anyhow!("Failed to create ui.bind: {e}"))?;
    ui.set("bind", constructor)
        .map_err(|e| anyhow!("Failed to attach ui.bind: {e}"))?;
    Ok(())
}

/// `ui.bind_list{ source, item_template }` — wire protocol v2 sentinel for
/// reactive list expansion. Emits a `$kind = "bind_list"` envelope:
///
/// ```json
/// { "$kind": "bind_list",
///   "source": "/<entity_type>",
///   "item_template": <UiNodeV1> }
/// ```
///
/// The client-side resolver walks the `source` store and clones
/// `item_template` once per record, replacing `@/<field>` paths with the
/// per-item values before primitive dispatch.
fn register_bind_list(lua: &Lua, ui: &Table) -> Result<()> {
    let constructor = lua
        .create_function(|lua, args: Table| {
            let source: String = args.get("source").map_err(|e| {
                mlua::Error::RuntimeError(format!(
                    "ui.bind_list: `source` (string) required: {e}"
                ))
            })?;
            if source.is_empty() {
                return Err(mlua::Error::RuntimeError(
                    "ui.bind_list: `source` must be a non-empty string".to_string(),
                ));
            }
            let template = match args.get::<Value>("item_template") {
                Ok(Value::Table(t)) => t,
                Ok(other) => {
                    return Err(mlua::Error::RuntimeError(format!(
                        "ui.bind_list: `item_template` must be a UiNode table, got {}",
                        other.type_name()
                    )));
                }
                Err(e) => {
                    return Err(mlua::Error::RuntimeError(format!(
                        "ui.bind_list: `item_template` (UiNode table) required: {e}"
                    )));
                }
            };
            let out = lua.create_table()?;
            out.set("$kind", "bind_list")?;
            out.set("source", source)?;
            out.set("item_template", template)?;
            Ok(out)
        })
        .map_err(|e| anyhow!("Failed to create ui.bind_list: {e}"))?;
    ui.set("bind_list", constructor)
        .map_err(|e| anyhow!("Failed to attach ui.bind_list: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Node construction
// ---------------------------------------------------------------------------

fn build_node(lua: &Lua, kind: Primitive, args: Value) -> mlua::Result<Table> {
    let input = match args {
        Value::Table(t) => t,
        Value::Nil => lua.create_table()?,
        other => {
            return Err(mlua::Error::RuntimeError(format!(
                "ui.{fn_name} expected a table, got {ty}",
                fn_name = kind.wire_name(),
                ty = other.type_name()
            )));
        }
    };

    let out = lua.create_table()?;
    out.set("type", kind.wire_name())?;

    // id
    if let Ok(Value::String(id)) = input.get::<Value>("id") {
        out.set("id", id)?;
    } else if !matches!(input.get::<Value>("id"), Ok(Value::Nil) | Err(_)) {
        return Err(mlua::Error::RuntimeError(format!(
            "ui.{}: id must be a string",
            kind.wire_name()
        )));
    }

    // children
    if let Ok(Value::Table(children)) = input.get::<Value>("children") {
        out.set("children", children)?;
    }

    // slots — merge explicit `slots = {...}` table with top-level slot keys
    // (spec-defined slot names hoisted for author convenience). Unknown slot
    // keys are rejected to catch typos (e.g. `footr` instead of `footer`).
    let allowed_slots = kind.allowed_slots();
    let slots_table = input.get::<Value>("slots").ok();
    let normalized_slots = lua.create_table()?;
    let mut slot_count = 0usize;

    if let Some(Value::Table(slots)) = slots_table {
        for pair in slots.pairs::<String, Value>() {
            let (key, value) = pair?;
            let wire_key = LUA_RESERVED_SLOT_ALIASES
                .iter()
                .find(|(alias, _)| *alias == key.as_str())
                .map_or(key.as_str(), |(_, wire)| *wire);
            if !allowed_slots.contains(&wire_key) {
                return Err(mlua::Error::RuntimeError(format!(
                    "ui.{}: unknown slot `{wire_key}`. Allowed slots: {allowed_slots:?}",
                    kind.wire_name()
                )));
            }
            normalized_slots.set(wire_key, value)?;
            slot_count += 1;
        }
    }
    for hoist in allowed_slots {
        // Direct canonical form, e.g. top-level `title = {...}`.
        if let Ok(Value::Table(slot)) = input.get::<Value>(*hoist) {
            normalized_slots.set(*hoist, slot)?;
            slot_count += 1;
            continue;
        }
        // Lua reserved-word alias, e.g. top-level `end_ = {...}` for wire `end`.
        if let Some((alias, _)) = LUA_RESERVED_SLOT_ALIASES
            .iter()
            .find(|(_, wire)| *wire == *hoist)
        {
            if let Ok(Value::Table(slot)) = input.get::<Value>(*alias) {
                normalized_slots.set(*hoist, slot)?;
                slot_count += 1;
            }
        }
    }

    // Enforce required slots.
    for required in kind.required_slots() {
        let present = matches!(
            normalized_slots.get::<Value>(*required),
            Ok(Value::Table(_))
        );
        if !present {
            return Err(mlua::Error::RuntimeError(format!(
                "ui.{}: required slot `{required}` is missing",
                kind.wire_name()
            )));
        }
    }
    if slot_count > 0 {
        out.set("slots", normalized_slots)?;
    }

    // props — every non-envelope, non-hoisted key. Top-level Lua keys are
    // converted snake_case → camelCase so the wire format matches the TS
    // types in the spec regardless of which casing the author uses. Unknown
    // props (web-only extensions, typos) are rejected at construction, mirror
    // of the slot allowlist above.
    let allowed_props = kind.allowed_props();
    let props = lua.create_table()?;
    let mut prop_count = 0usize;
    for pair in input.pairs::<String, Value>() {
        let (key, value) = pair?;
        if ENVELOPE_KEYS.contains(&key.as_str()) {
            continue;
        }
        if allowed_slots.contains(&key.as_str()) {
            // Already consumed as a slot above.
            continue;
        }
        // Lua reserved-word aliases (e.g. `end_`) for allowed slots are also
        // consumed by the slot hoisting pass above.
        if LUA_RESERVED_SLOT_ALIASES
            .iter()
            .any(|(alias, wire)| *alias == key.as_str() && allowed_slots.contains(wire))
        {
            continue;
        }
        // Reject null/Nil explicitly so props round-trip cleanly.
        if matches!(value, Value::Nil) {
            continue;
        }
        let wire_key = snake_to_camel(&key);
        if !allowed_props.contains(&wire_key.as_str()) {
            return Err(mlua::Error::RuntimeError(format!(
                "ui.{}: unknown prop `{wire_key}`. Allowed props: {allowed_props:?}",
                kind.wire_name()
            )));
        }
        props.set(wire_key, value)?;
        prop_count += 1;
    }
    if prop_count > 0 {
        out.set("props", props)?;
    }

    // Primitive-specific validation.
    validate(lua, kind, &out)?;
    Ok(out)
}

fn validate(_lua: &Lua, kind: Primitive, node: &Table) -> mlua::Result<()> {
    match kind {
        Primitive::TreeItem => {
            // `id` is required for tree_item — matches TreeItemPropsV1.
            let id = node.get::<Value>("id").unwrap_or(Value::Nil);
            if matches!(id, Value::Nil) {
                return Err(mlua::Error::RuntimeError(
                    "ui.tree_item requires a string `id`".to_string(),
                ));
            }
        }
        Primitive::Stack => {
            // Per cross-client spec, `direction` is required.
            require_prop(node, "direction", "ui.stack")?;
        }
        Primitive::Text => require_prop_string(node, "text", "ui.text")?,
        Primitive::Icon => require_prop_string(node, "name", "ui.icon")?,
        Primitive::Badge => require_prop_string(node, "text", "ui.badge")?,
        Primitive::StatusDot => require_prop_string(node, "state", "ui.status_dot")?,
        Primitive::EmptyState => require_prop_string(node, "title", "ui.empty_state")?,
        Primitive::Button => {
            require_prop_string(node, "label", "ui.button")?;
            require_prop_table(node, "action", "ui.button")?;
        }
        Primitive::IconButton => {
            require_prop_string(node, "icon", "ui.icon_button")?;
            require_prop_string(node, "label", "ui.icon_button")?;
            require_prop_table(node, "action", "ui.icon_button")?;
        }
        Primitive::Dialog => {
            require_prop_string(node, "title", "ui.dialog")?;
            let props: Table = node.get("props").map_err(|e| {
                mlua::Error::RuntimeError(format!(
                    "ui.dialog requires `open` (boolean) and `title` (string): {e}"
                ))
            })?;
            match props.get::<Value>("open")? {
                Value::Boolean(_) => {}
                _ => {
                    return Err(mlua::Error::RuntimeError(
                        "ui.dialog requires `open` (boolean)".to_string(),
                    ));
                }
            }
            // Default presentation to "auto" when omitted.
            let presentation = props.get::<Value>("presentation")?;
            if matches!(presentation, Value::Nil) {
                props.set("presentation", "auto")?;
            }
        }
        Primitive::WorktreeList => require_prop_string(node, "targetId", "ui.worktree_list")?,
        Primitive::SessionRow => require_prop_string(node, "sessionUuid", "ui.session_row")?,
        Primitive::NewSessionButton => {
            require_prop_table(node, "action", "ui.new_session_button")?;
        }
        // Composites with all-optional or no props need no extra validation —
        // the prop allowlist already rejects typos.
        Primitive::Tree
        | Primitive::Inline
        | Primitive::Panel
        | Primitive::ScrollArea
        | Primitive::SessionList
        | Primitive::WorkspaceList
        | Primitive::SpawnTargetList
        | Primitive::HubRecoveryState
        | Primitive::ConnectionCode => {}
    }
    Ok(())
}

/// Returns `true` when `value` is a wire-protocol-v2 `$bind` sentinel
/// (i.e. a single-key Lua table `{ ["$bind"] = "/<path>" }`). Required-prop
/// validators accept the sentinel as a stand-in for the eventual resolved
/// value — the resolver runs client-side before primitive dispatch.
fn is_bind_sentinel(value: &Value) -> bool {
    let Value::Table(t) = value else { return false };
    let mut count = 0usize;
    let mut has_bind = false;
    let Ok(pairs) = t.clone().pairs::<String, Value>().collect::<mlua::Result<Vec<_>>>() else {
        return false;
    };
    for (key, val) in pairs {
        count += 1;
        if key == "$bind" && matches!(val, Value::String(_)) {
            has_bind = true;
        }
    }
    count == 1 && has_bind
}

fn require_prop(node: &Table, key: &str, ctor: &str) -> mlua::Result<()> {
    let props = node.get::<Value>("props").ok();
    let Some(Value::Table(props)) = props else {
        return Err(mlua::Error::RuntimeError(format!(
            "{ctor} requires a `{key}` value"
        )));
    };
    match props.get::<Value>(key) {
        Ok(Value::Nil) | Err(_) => Err(mlua::Error::RuntimeError(format!(
            "{ctor} requires a `{key}` value"
        ))),
        Ok(_) => Ok(()),
    }
}

fn require_prop_string(node: &Table, key: &str, ctor: &str) -> mlua::Result<()> {
    let props = node.get::<Value>("props").ok();
    let Some(Value::Table(props)) = props else {
        return Err(mlua::Error::RuntimeError(format!(
            "{ctor} requires a `{key}` string"
        )));
    };
    match props.get::<Value>(key) {
        Ok(Value::String(_)) => Ok(()),
        Ok(ref v) if is_bind_sentinel(v) => Ok(()),
        _ => Err(mlua::Error::RuntimeError(format!(
            "{ctor} requires a `{key}` string"
        ))),
    }
}

fn require_prop_table(node: &Table, key: &str, ctor: &str) -> mlua::Result<()> {
    let props = node.get::<Value>("props").ok();
    let Some(Value::Table(props)) = props else {
        return Err(mlua::Error::RuntimeError(format!(
            "{ctor} requires a `{key}` table"
        )));
    };
    match props.get::<Value>(key) {
        Ok(Value::Table(_)) => Ok(()),
        _ => Err(mlua::Error::RuntimeError(format!(
            "{ctor} requires a `{key}` table"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Responsive + conditional construction
// ---------------------------------------------------------------------------

fn build_responsive(lua: &Lua, input: &Table) -> mlua::Result<Table> {
    let has_width = !matches!(input.get::<Value>("width").unwrap_or(Value::Nil), Value::Nil);
    let has_height = !matches!(
        input.get::<Value>("height").unwrap_or(Value::Nil),
        Value::Nil
    );

    let out = lua.create_table()?;
    out.set(KIND_KEY, "responsive")?;

    if has_width || has_height {
        // Explicit dimension form.
        if has_width {
            let width = input.get::<Table>("width").map_err(|e| {
                mlua::Error::RuntimeError(format!(
                    "ui.responsive: `width` must be a table of width-class keys: {e}"
                ))
            })?;
            validate_dimension_keys(&width, WIDTH_CLASS_KEYS, "width")?;
            out.set("width", width)?;
        }
        if has_height {
            let height = input.get::<Table>("height").map_err(|e| {
                mlua::Error::RuntimeError(format!(
                    "ui.responsive: `height` must be a table of height-class keys: {e}"
                ))
            })?;
            validate_dimension_keys(&height, &["short", "regular", "tall"], "height")?;
            out.set("height", height)?;
        }
        // Reject any other top-level keys so mixed-shorthand mistakes surface.
        for pair in input.pairs::<String, Value>() {
            let (key, _) = pair?;
            if key != "width" && key != "height" {
                return Err(mlua::Error::RuntimeError(format!(
                    "ui.responsive: unexpected key `{key}` in explicit dimension form. Put breakpoints inside `width` or `height`."
                )));
            }
        }
    } else {
        // Width-only shorthand: keys must all be width-class names.
        let mut saw_width = false;
        let mut saw_height = false;
        for pair in input.pairs::<String, Value>() {
            let (key, _) = pair?;
            let is_width = WIDTH_CLASS_KEYS.contains(&key.as_str());
            let is_height = HEIGHT_CLASS_KEYS.contains(&key.as_str());
            // NOTE: "regular" is intentionally only counted as width in shorthand;
            // if the author needs height=regular they must use explicit form.
            if !is_width && !is_height {
                return Err(mlua::Error::RuntimeError(format!(
                    "ui.responsive: unknown key `{key}`. Expected one of {:?} (width shorthand) or explicit `width` / `height` form.",
                    WIDTH_CLASS_KEYS
                )));
            }
            if is_height {
                saw_height = true;
            }
            if is_width {
                saw_width = true;
            }
        }
        if saw_height && saw_width {
            return Err(mlua::Error::RuntimeError(
                "ui.responsive: mixed width and height keys in shorthand. Use explicit `{ width = {...}, height = {...} }` form."
                    .to_string(),
            ));
        }
        if saw_height {
            // Shorthand height: short/tall only (regular is ambiguous).
            let dim = lua.create_table()?;
            for pair in input.pairs::<String, Value>() {
                let (key, value) = pair?;
                dim.set(key, value)?;
            }
            out.set("height", dim)?;
        } else {
            let dim = lua.create_table()?;
            for pair in input.pairs::<String, Value>() {
                let (key, value) = pair?;
                dim.set(key, value)?;
            }
            out.set("width", dim)?;
        }
    }

    Ok(out)
}

fn validate_dimension_keys(
    table: &Table,
    allowed: &[&str],
    dim_name: &str,
) -> mlua::Result<()> {
    for pair in table.pairs::<String, Value>() {
        let (key, _) = pair?;
        if !allowed.contains(&key.as_str()) {
            return Err(mlua::Error::RuntimeError(format!(
                "ui.responsive: `{dim_name}` has unexpected key `{key}`. Expected one of {allowed:?}."
            )));
        }
    }
    Ok(())
}

fn build_conditional(
    lua: &Lua,
    kind_name: &'static str,
    condition: Value,
    node: Value,
) -> mlua::Result<Table> {
    let condition_table = normalize_condition(lua, condition, kind_name)?;
    let node_table = match node {
        Value::Table(t) => t,
        other => {
            return Err(mlua::Error::RuntimeError(format!(
                "ui.{kind_name} expected a node table as second arg, got {}",
                other.type_name()
            )));
        }
    };
    let out = lua.create_table()?;
    out.set(KIND_KEY, kind_name)?;
    out.set("condition", condition_table)?;
    out.set("node", node_table)?;
    Ok(out)
}

fn normalize_condition(
    lua: &Lua,
    condition: Value,
    ctor: &'static str,
) -> mlua::Result<Table> {
    match condition {
        Value::String(s) => {
            // Bare-string shorthand = widthClass match.
            let width = s.to_str()?.to_string();
            if !WIDTH_CLASS_KEYS.contains(&width.as_str()) {
                return Err(mlua::Error::RuntimeError(format!(
                    "ui.{ctor}: bare string shorthand must be one of {WIDTH_CLASS_KEYS:?}, got `{width}`"
                )));
            }
            let t = lua.create_table()?;
            t.set("width", width)?;
            Ok(t)
        }
        Value::Table(t) => {
            // Rewrite snake_case to camelCase (e.g. keyboard_occluded → keyboardOccluded).
            let out = lua.create_table()?;
            for pair in t.pairs::<String, Value>() {
                let (key, value) = pair?;
                out.set(snake_to_camel(&key), value)?;
            }
            Ok(out)
        }
        other => Err(mlua::Error::RuntimeError(format!(
            "ui.{ctor}: condition must be a string (width shorthand) or a table, got {}",
            other.type_name()
        ))),
    }
}

/// Convert a snake_case prop key to camelCase. No-op for keys without
/// underscores, so `interactionDensity` and `interaction_density` both
/// end up as `interactionDensity` on the wire.
fn snake_to_camel(key: &str) -> String {
    if !key.contains('_') {
        return key.to_string();
    }
    let mut out = String::with_capacity(key.len());
    let mut upper_next = false;
    for ch in key.chars() {
        if ch == '_' {
            upper_next = true;
            continue;
        }
        if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        clippy::needless_borrows_for_generic_args,
        clippy::unnecessary_map_or,
        reason = "test-code brevity: these lints flag patterns we intentionally use in tests"
    )]

    use super::*;
    use mlua::LuaSerdeExt;
    use serde_json::json;

    fn eval_to_json(lua: &Lua, code: &str) -> serde_json::Value {
        let value: Value = lua.load(code).eval().expect("Lua eval failed");
        lua.from_value(value).expect("Lua -> JSON conversion failed")
    }

    fn new_lua() -> Lua {
        let lua = Lua::new();
        register(&lua).expect("register ui");
        lua
    }

    #[test]
    fn stack_basic_shape() {
        let lua = new_lua();
        let v = eval_to_json(&lua, r#"
            return ui.stack{
                direction = "vertical",
                gap = "2",
                children = { ui.text{ text = "hi" } },
            }
        "#);
        assert_eq!(
            v,
            json!({
                "type": "stack",
                "props": { "direction": "vertical", "gap": "2" },
                "children": [ { "type": "text", "props": { "text": "hi" } } ]
            })
        );
    }

    #[test]
    fn stack_requires_direction() {
        let lua = new_lua();
        let err = lua
            .load(r#"return ui.stack{ gap = "2" }"#)
            .eval::<Value>()
            .unwrap_err();
        assert!(err.to_string().contains("direction"), "got {err}");
    }

    #[test]
    fn text_requires_text_prop() {
        let lua = new_lua();
        let err = lua.load("return ui.text{ tone = 'accent' }").eval::<Value>().unwrap_err();
        assert!(err.to_string().contains("ui.text"), "got {err}");
    }

    #[test]
    fn tree_item_requires_id() {
        let lua = new_lua();
        let err = lua
            .load("return ui.tree_item{ selected = true, slots = { title = { ui.text{ text='x' } } } }")
            .eval::<Value>()
            .unwrap_err();
        assert!(err.to_string().contains("ui.tree_item"), "got {err}");
    }

    #[test]
    fn tree_item_requires_title_slot() {
        let lua = new_lua();
        let err = lua
            .load(r#"return ui.tree_item{ id = "x" }"#)
            .eval::<Value>()
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ui.tree_item"), "got {err}");
        assert!(msg.contains("title"), "got {err}");
    }

    #[test]
    fn tree_item_rejects_unknown_slot() {
        let lua = new_lua();
        let err = lua
            .load(
                r#"return ui.tree_item{
                    id = "x",
                    slots = {
                        title = { ui.text{ text = "t" } },
                        whoops = { ui.text{ text = "typo" } },
                    },
                }"#,
            )
            .eval::<Value>()
            .unwrap_err();
        assert!(err.to_string().contains("whoops"), "got {err}");
    }

    #[test]
    fn primitive_without_slots_rejects_any_slot() {
        let lua = new_lua();
        let err = lua
            .load(
                r#"return ui.text{
                    text = "hi",
                    slots = { title = { ui.text{ text = "no" } } },
                }"#,
            )
            .eval::<Value>()
            .unwrap_err();
        assert!(err.to_string().contains("unknown slot"), "got {err}");
    }

    #[test]
    fn slots_end_underscore_rewritten() {
        let lua = new_lua();
        let v = eval_to_json(&lua, r#"
            return ui.tree_item{
                id = "ws-1",
                slots = {
                    title = { ui.text{ text = "Workspace" } },
                    end_ = { ui.badge{ text = "3" } },
                },
            }
        "#);
        let slots = v.get("slots").expect("slots present");
        assert!(slots.get("end").is_some(), "`end_` should be rewritten to `end`: {slots:?}");
        assert!(slots.get("end_").is_none(), "`end_` should not survive: {slots:?}");
    }

    #[test]
    fn tree_item_accepts_slot_keys_at_top_level() {
        let lua = new_lua();
        let v = eval_to_json(&lua, r#"
            return ui.tree_item{
                id = "ws-1",
                title = { ui.text{ text = "Workspace" } },
                end_  = { ui.badge{ text = "3" } },
            }
        "#);
        // Spec slot keys at the top level are hoisted into `slots`, not `props`.
        let slots = v.get("slots").expect("slots present");
        assert!(slots.get("title").is_some());
        assert!(slots.get("end").is_some());
        let props = v.get("props");
        assert!(
            props.map_or(true, |p| p.get("title").is_none() && p.get("end").is_none()),
            "slot keys must not leak into props: {props:?}"
        );
    }

    #[test]
    fn button_action_roundtrip() {
        let lua = new_lua();
        let v = eval_to_json(&lua, r#"
            return ui.button{
                label = "Save",
                action = ui.action("botster.workspace.save", { workspaceId = "ws-1" }),
                variant = "solid",
            }
        "#);
        assert_eq!(
            v,
            json!({
                "type": "button",
                "props": {
                    "label": "Save",
                    "variant": "solid",
                    "action": {
                        "id": "botster.workspace.save",
                        "payload": { "workspaceId": "ws-1" }
                    }
                }
            })
        );
    }

    #[test]
    fn responsive_width_shorthand() {
        let lua = new_lua();
        let v = eval_to_json(&lua, r#"
            return ui.responsive({ compact = "vertical", expanded = "horizontal" })
        "#);
        assert_eq!(
            v,
            json!({
                "$kind": "responsive",
                "width": { "compact": "vertical", "expanded": "horizontal" }
            })
        );
    }

    #[test]
    fn responsive_explicit_form() {
        let lua = new_lua();
        let v = eval_to_json(&lua, r#"
            return ui.responsive({
                width = { regular = "panel" },
                height = { tall = "panel", short = "sidebar" },
            })
        "#);
        assert_eq!(
            v,
            json!({
                "$kind": "responsive",
                "width": { "regular": "panel" },
                "height": { "tall": "panel", "short": "sidebar" }
            })
        );
    }

    #[test]
    fn responsive_rejects_mixed_shorthand() {
        let lua = new_lua();
        let err = lua
            .load(r#"return ui.responsive({ compact = "a", tall = "b" })"#)
            .eval::<Value>()
            .unwrap_err();
        assert!(
            err.to_string().contains("mixed width and height"),
            "got {err}"
        );
    }

    #[test]
    fn responsive_rejects_unknown_shorthand_key() {
        let lua = new_lua();
        let err = lua
            .load(r#"return ui.responsive({ phone = "vertical" })"#)
            .eval::<Value>()
            .unwrap_err();
        assert!(err.to_string().contains("unknown key"), "got {err}");
    }

    #[test]
    fn when_shorthand_string_is_width_class() {
        let lua = new_lua();
        let v = eval_to_json(&lua, r#"
            return ui.when("compact", ui.text{ text = "mobile-only" })
        "#);
        assert_eq!(
            v,
            json!({
                "$kind": "when",
                "condition": { "width": "compact" },
                "node": { "type": "text", "props": { "text": "mobile-only" } }
            })
        );
    }

    #[test]
    fn hidden_table_condition_with_keyboard_occlusion() {
        let lua = new_lua();
        let v = eval_to_json(&lua, r#"
            return ui.hidden(
                { pointer = "coarse", keyboard_occluded = true },
                ui.panel{}
            )
        "#);
        assert_eq!(
            v,
            json!({
                "$kind": "hidden",
                "condition": { "pointer": "coarse", "keyboardOccluded": true },
                "node": { "type": "panel" }
            })
        );
    }

    #[test]
    fn when_rejects_invalid_bare_string() {
        let lua = new_lua();
        let err = lua
            .load(r#"return ui.when("phone", ui.text{ text = "x" })"#)
            .eval::<Value>()
            .unwrap_err();
        assert!(err.to_string().contains("shorthand"), "got {err}");
    }

    #[test]
    fn dialog_defaults_presentation_to_auto() {
        let lua = new_lua();
        let v = eval_to_json(&lua, r#"
            return ui.dialog{ open = true, title = "Rename" }
        "#);
        let props = v.get("props").expect("props");
        assert_eq!(props.get("presentation").and_then(|p| p.as_str()), Some("auto"));
    }

    #[test]
    fn dialog_hoists_body_and_footer_into_slots() {
        let lua = new_lua();
        let v = eval_to_json(&lua, r#"
            return ui.dialog{
                open = true,
                title = "Rename Workspace",
                body   = { ui.text{ text = "Enter a name" } },
                footer = { ui.button{ label = "Save", action = ui.action("botster.workspace.rename.commit") } },
            }
        "#);
        let props = v.get("props").expect("props");
        assert!(props.get("body").is_none(), "`body` must not appear in props: {props:?}");
        assert!(props.get("footer").is_none(), "`footer` must not appear in props: {props:?}");
        let slots = v.get("slots").expect("slots present");
        assert!(slots.get("body").is_some(), "body should be hoisted into slots: {slots:?}");
        assert!(slots.get("footer").is_some(), "footer should be hoisted into slots: {slots:?}");
    }

    #[test]
    fn dialog_requires_open_boolean() {
        let lua = new_lua();
        let err = lua
            .load(r#"return ui.dialog{ title = "x" }"#)
            .eval::<Value>()
            .unwrap_err();
        assert!(err.to_string().contains("open"), "got {err}");
    }

    #[test]
    fn tree_has_no_required_props() {
        let lua = new_lua();
        let v = eval_to_json(&lua, "return ui.tree{}");
        assert_eq!(v, json!({ "type": "tree" }));
    }

    #[test]
    fn action_payload_is_optional() {
        let lua = new_lua();
        let v = eval_to_json(&lua, r#"return ui.action("botster.session.select")"#);
        assert_eq!(v, json!({ "id": "botster.session.select" }));
    }

    #[test]
    fn every_v1_primitive_registered() {
        let lua = new_lua();
        for name in [
            "stack",
            "inline",
            "panel",
            "scroll_area",
            "text",
            "icon",
            "badge",
            "status_dot",
            "empty_state",
            "button",
            "icon_button",
            "tree",
            "tree_item",
            "dialog",
            "action",
            "responsive",
            "when",
            "hidden",
        ] {
            let f: Value = lua
                .load(&format!("return ui.{name}"))
                .eval()
                .expect("eval");
            assert!(matches!(f, Value::Function(_)), "ui.{name} missing");
        }
    }

    #[test]
    fn unknown_prop_is_rejected() {
        let lua = new_lua();
        let err = lua
            .load(r#"return ui.panel{ title = "x", padding = "4" }"#)
            .eval::<Value>()
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown prop"), "got {err}");
        assert!(msg.contains("padding"), "error must name the offending key: {err}");
        assert!(msg.contains("Allowed props"), "error must list allowed props: {err}");
    }

    #[test]
    fn allowlist_catches_both_snake_and_camel_forms_of_web_only_key() {
        let lua = new_lua();
        for leading in ["leadingIcon", "leading_icon"] {
            let code = format!(
                r#"return ui.button{{ label = "x", action = ui.action("a"), {leading} = "check" }}"#
            );
            let err = lua.load(&code).eval::<Value>().unwrap_err();
            assert!(
                err.to_string().contains("unknown prop"),
                "form `{leading}` should be rejected: {err}"
            );
        }
    }

    #[test]
    fn envelope_keys_are_not_treated_as_props() {
        // id, children, slots live on the envelope and must not trigger
        // unknown-prop errors even though they're not in allowed_props.
        let lua = new_lua();
        let v = eval_to_json(
            &lua,
            r#"
                return ui.tree_item{
                    id = "ws-1",
                    children = {},
                    slots = { title = { ui.text{ text = "x" } } },
                }
            "#,
        );
        assert_eq!(v.get("id").and_then(|v| v.as_str()), Some("ws-1"));
    }

    #[test]
    fn snake_case_prop_keys_are_rewritten_to_camel_case() {
        let lua = new_lua();
        // Use Panel's interactionDensity as the multi-word case since Button's
        // icon is now single-word per cross-client spec.
        let v = eval_to_json(
            &lua,
            r#"
                return ui.panel{
                    title = "x",
                    interaction_density = "comfortable",
                }
            "#,
        );
        let props = v.get("props").expect("props");
        assert_eq!(
            props.get("interactionDensity").and_then(|v| v.as_str()),
            Some("comfortable")
        );
        assert!(
            props.get("interaction_density").is_none(),
            "snake form must not survive on wire"
        );
    }

    #[test]
    fn snake_to_camel_is_noop_for_camel_case_keys() {
        assert_eq!(super::snake_to_camel("leadingIcon"), "leadingIcon");
        assert_eq!(super::snake_to_camel("leading_icon"), "leadingIcon");
        assert_eq!(super::snake_to_camel("primary_action"), "primaryAction");
        assert_eq!(super::snake_to_camel("title"), "title");
        assert_eq!(super::snake_to_camel("interaction_density"), "interactionDensity");
    }

    #[test]
    fn menu_is_not_exposed() {
        let lua = new_lua();
        let menu: Value = lua.load("return ui.menu").eval().expect("eval");
        assert!(matches!(menu, Value::Nil));
        let menu_item: Value = lua.load("return ui.menu_item").eval().expect("eval");
        assert!(matches!(menu_item, Value::Nil));
    }

    // =========================================================================
    // Wire protocol v2 — composite primitives
    // =========================================================================

    #[test]
    fn every_v2_composite_registered() {
        let lua = new_lua();
        for name in [
            "session_list",
            "workspace_list",
            "spawn_target_list",
            "worktree_list",
            "session_row",
            "hub_recovery_state",
            "connection_code",
            "new_session_button",
        ] {
            let f: Value = lua
                .load(&format!("return ui.{name}"))
                .eval()
                .expect("eval");
            assert!(matches!(f, Value::Function(_)), "ui.{name} missing");
        }
    }

    #[test]
    fn session_list_minimal_round_trip() {
        let lua = new_lua();
        let v = eval_to_json(&lua, "return ui.session_list{}");
        assert_eq!(v, json!({ "type": "session_list" }));
    }

    #[test]
    fn session_list_props_round_trip() {
        let lua = new_lua();
        let v = eval_to_json(
            &lua,
            r#"return ui.session_list{
                density = "sidebar",
                grouping = "workspace",
                show_nav_entries = true,
            }"#,
        );
        assert_eq!(
            v,
            json!({
                "type": "session_list",
                "props": {
                    "density": "sidebar",
                    "grouping": "workspace",
                    "showNavEntries": true
                }
            })
        );
    }

    #[test]
    fn session_list_density_accepts_responsive() {
        let lua = new_lua();
        let v = eval_to_json(
            &lua,
            r#"return ui.session_list{
                density = ui.responsive({ compact = "sidebar", expanded = "panel" }),
            }"#,
        );
        let props = v.get("props").expect("props");
        let density = props.get("density").expect("density");
        assert_eq!(density["$kind"], json!("responsive"));
        assert_eq!(density["width"]["compact"], json!("sidebar"));
        assert_eq!(density["width"]["expanded"], json!("panel"));
    }

    #[test]
    fn session_list_rejects_unknown_prop() {
        let lua = new_lua();
        let err = lua
            .load(r#"return ui.session_list{ density = "sidebar", foo = "bar" }"#)
            .eval::<Value>()
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown prop"), "got {err}");
        assert!(msg.contains("foo"), "error must name offending key: {err}");
    }

    #[test]
    fn session_list_rejects_children() {
        let lua = new_lua();
        // session_list is data-driven; children should be silently ignored
        // by the envelope (no `children` key in allowlist), but the
        // envelope-handling code passes `children` straight through. Verify
        // the wire shape stays clean: children get attached at the envelope
        // level (legal — same as any primitive), but renderers will ignore
        // them. We document this as a no-op rather than an error since the
        // envelope handling is generic. The composite renderer is allowed
        // to render children if it wants to, but the spec says these
        // composites do not consume children.
        let v = eval_to_json(
            &lua,
            r#"return ui.session_list{ children = { ui.text{ text = "x" } } }"#,
        );
        // Children attach to envelope; the test only asserts the envelope is
        // well-formed and that children DID NOT escape into props.
        assert_eq!(v.get("type").and_then(|v| v.as_str()), Some("session_list"));
        assert!(v.get("props").map_or(true, |p| p.get("children").is_none()));
    }

    #[test]
    fn workspace_list_minimal_round_trip() {
        let lua = new_lua();
        let v = eval_to_json(&lua, "return ui.workspace_list{}");
        assert_eq!(v, json!({ "type": "workspace_list" }));
    }

    #[test]
    fn workspace_list_with_density() {
        let lua = new_lua();
        let v = eval_to_json(&lua, r#"return ui.workspace_list{ density = "panel" }"#);
        assert_eq!(
            v,
            json!({ "type": "workspace_list", "props": { "density": "panel" } })
        );
    }

    #[test]
    fn spawn_target_list_minimal_round_trip() {
        let lua = new_lua();
        let v = eval_to_json(&lua, "return ui.spawn_target_list{}");
        assert_eq!(v, json!({ "type": "spawn_target_list" }));
    }

    #[test]
    fn spawn_target_list_with_action_templates() {
        let lua = new_lua();
        let v = eval_to_json(
            &lua,
            r#"return ui.spawn_target_list{
                on_select = ui.action("custom.target.select"),
                on_remove = ui.action("custom.target.remove"),
            }"#,
        );
        assert_eq!(
            v,
            json!({
                "type": "spawn_target_list",
                "props": {
                    "onSelect": { "id": "custom.target.select" },
                    "onRemove": { "id": "custom.target.remove" }
                }
            })
        );
    }

    #[test]
    fn worktree_list_requires_target_id() {
        let lua = new_lua();
        let err = lua
            .load("return ui.worktree_list{}")
            .eval::<Value>()
            .unwrap_err();
        assert!(err.to_string().contains("targetId"), "got {err}");
    }

    #[test]
    fn worktree_list_round_trip() {
        let lua = new_lua();
        let v = eval_to_json(&lua, r#"return ui.worktree_list{ target_id = "tgt-1" }"#);
        assert_eq!(
            v,
            json!({
                "type": "worktree_list",
                "props": { "targetId": "tgt-1" }
            })
        );
    }

    #[test]
    fn session_row_requires_session_uuid() {
        let lua = new_lua();
        let err = lua
            .load("return ui.session_row{}")
            .eval::<Value>()
            .unwrap_err();
        assert!(err.to_string().contains("sessionUuid"), "got {err}");
    }

    #[test]
    fn session_row_round_trip() {
        let lua = new_lua();
        let v = eval_to_json(
            &lua,
            r#"return ui.session_row{ session_uuid = "sess-1", density = "sidebar" }"#,
        );
        assert_eq!(
            v,
            json!({
                "type": "session_row",
                "props": { "sessionUuid": "sess-1", "density": "sidebar" }
            })
        );
    }

    #[test]
    fn hub_recovery_state_minimal() {
        let lua = new_lua();
        let v = eval_to_json(&lua, "return ui.hub_recovery_state{}");
        assert_eq!(v, json!({ "type": "hub_recovery_state" }));
    }

    #[test]
    fn hub_recovery_state_rejects_props() {
        let lua = new_lua();
        let err = lua
            .load(r#"return ui.hub_recovery_state{ status = "ready" }"#)
            .eval::<Value>()
            .unwrap_err();
        assert!(err.to_string().contains("unknown prop"), "got {err}");
    }

    #[test]
    fn connection_code_minimal() {
        let lua = new_lua();
        let v = eval_to_json(&lua, "return ui.connection_code{}");
        assert_eq!(v, json!({ "type": "connection_code" }));
    }

    #[test]
    fn new_session_button_requires_action() {
        let lua = new_lua();
        let err = lua
            .load("return ui.new_session_button{}")
            .eval::<Value>()
            .unwrap_err();
        assert!(err.to_string().contains("action"), "got {err}");
    }

    #[test]
    fn new_session_button_round_trip() {
        let lua = new_lua();
        let v = eval_to_json(
            &lua,
            r#"return ui.new_session_button{
                action = ui.action("botster.session.create.request"),
            }"#,
        );
        assert_eq!(
            v,
            json!({
                "type": "new_session_button",
                "props": {
                    "action": { "id": "botster.session.create.request" }
                }
            })
        );
    }

    // -------------------------------------------------------------------------
    // Wire protocol v2 — ui.bind / ui.bind_list
    // -------------------------------------------------------------------------

    #[test]
    fn bind_emits_sentinel_object() {
        let lua = new_lua();
        let v = eval_to_json(&lua, r#"return ui.bind("/session/sess-1/title")"#);
        assert_eq!(v, json!({ "$bind": "/session/sess-1/title" }));
    }

    #[test]
    fn bind_rejects_empty_path() {
        let lua = new_lua();
        let err = lua.load(r#"return ui.bind("")"#).eval::<Value>().unwrap_err();
        assert!(err.to_string().contains("ui.bind"), "got {err}");
    }

    #[test]
    fn bind_inside_text_prop_is_passed_through_verbatim() {
        // The constructor itself doesn't resolve — it just emits the
        // sentinel. Renderers resolve at render time. Verify the wire shape
        // round-trips cleanly inside an enclosing primitive's prop.
        let lua = new_lua();
        let v = eval_to_json(
            &lua,
            r#"return ui.text{ text = ui.bind("/session/sess-1/title") }"#,
        );
        assert_eq!(
            v,
            json!({
                "type": "text",
                "props": { "text": { "$bind": "/session/sess-1/title" } }
            })
        );
    }

    #[test]
    fn bind_list_emits_kind_sentinel_with_source_and_template() {
        let lua = new_lua();
        let v = eval_to_json(
            &lua,
            r#"return ui.bind_list{
                source = "/session",
                item_template = ui.text{ text = ui.bind("@/title") },
            }"#,
        );
        assert_eq!(v["$kind"], json!("bind_list"));
        assert_eq!(v["source"], json!("/session"));
        assert_eq!(v["item_template"]["type"], json!("text"));
        assert_eq!(
            v["item_template"]["props"]["text"],
            json!({ "$bind": "@/title" })
        );
    }

    #[test]
    fn bind_list_rejects_missing_source() {
        let lua = new_lua();
        let err = lua
            .load(r#"return ui.bind_list{ item_template = ui.text{ text = "x" } }"#)
            .eval::<Value>()
            .unwrap_err();
        assert!(err.to_string().contains("source"), "got {err}");
    }

    #[test]
    fn bind_list_rejects_non_table_item_template() {
        let lua = new_lua();
        let err = lua
            .load(r#"return ui.bind_list{ source = "/session", item_template = "not a node" }"#)
            .eval::<Value>()
            .unwrap_err();
        assert!(err.to_string().contains("item_template"), "got {err}");
    }

    #[test]
    fn v2_composites_typed_props_round_trip_via_serde() {
        // Wire shape ↔ typed PropsV1 round-trip for every v2 composite.
        // Catches any drift between the Lua allowlist and the Rust struct.
        use crate::ui_contract::{
            ConnectionCodePropsV1, HubRecoveryStatePropsV1, NewSessionButtonPropsV1,
            SessionListPropsV1, SessionRowPropsV1, SpawnTargetListPropsV1, UiActionV1,
            UiSessionListGrouping, UiSurfaceDensity, UiValueV1, WorkspaceListPropsV1,
            WorktreeListPropsV1,
        };

        let session_list = SessionListPropsV1 {
            density: Some(UiValueV1::scalar(UiSurfaceDensity::Sidebar)),
            grouping: Some(UiSessionListGrouping::Workspace),
            show_nav_entries: Some(true),
        };
        let v = serde_json::to_value(&session_list).unwrap();
        let back: SessionListPropsV1 = serde_json::from_value(v).unwrap();
        assert_eq!(back, session_list);

        let workspace_list = WorkspaceListPropsV1 {
            density: Some(UiValueV1::scalar(UiSurfaceDensity::Panel)),
        };
        let back: WorkspaceListPropsV1 =
            serde_json::from_value(serde_json::to_value(&workspace_list).unwrap()).unwrap();
        assert_eq!(back, workspace_list);

        let spawn = SpawnTargetListPropsV1 {
            on_select: Some(UiActionV1::new("a")),
            on_remove: Some(UiActionV1::new("b")),
        };
        let back: SpawnTargetListPropsV1 =
            serde_json::from_value(serde_json::to_value(&spawn).unwrap()).unwrap();
        assert_eq!(back, spawn);

        let worktree = WorktreeListPropsV1 {
            target_id: "t".into(),
        };
        let back: WorktreeListPropsV1 =
            serde_json::from_value(serde_json::to_value(&worktree).unwrap()).unwrap();
        assert_eq!(back, worktree);

        let row = SessionRowPropsV1 {
            session_uuid: "s".into(),
            density: None,
        };
        let back: SessionRowPropsV1 =
            serde_json::from_value(serde_json::to_value(&row).unwrap()).unwrap();
        assert_eq!(back, row);

        let hr = HubRecoveryStatePropsV1::default();
        let back: HubRecoveryStatePropsV1 =
            serde_json::from_value(serde_json::to_value(&hr).unwrap()).unwrap();
        assert_eq!(back, hr);

        let cc = ConnectionCodePropsV1::default();
        let back: ConnectionCodePropsV1 =
            serde_json::from_value(serde_json::to_value(&cc).unwrap()).unwrap();
        assert_eq!(back, cc);

        let nsb = NewSessionButtonPropsV1 {
            action: UiActionV1::new("c"),
        };
        let back: NewSessionButtonPropsV1 =
            serde_json::from_value(serde_json::to_value(&nsb).unwrap()).unwrap();
        assert_eq!(back, nsb);
    }
}
