# Elements Components Migration Audit

Comprehensive audit of migrating trybotster UI to `tailwindplus_elements_components`.

---

## Table of Contents

1. [Setup Status](#1-setup-status)
2. [Color Philosophy](#2-color-philosophy)
3. [Component APIs](#3-component-apis)
4. [File-by-File Audit](#4-file-by-file-audit)
5. [JavaScript Changes](#5-javascript-changes)
6. [New Files to Add](#6-new-files-to-add)
7. [Testing Checklist](#7-testing-checklist)

---

## 1. Setup Status

| Task | Status | File |
|------|--------|------|
| Sync gem files | Done | 22 files in `app/components/elements/` |
| Add gems | Done | `view_component`, `classy-yaml` in Gemfile |
| Create initializer | Done | `config/initializers/classy_yaml.rb` |
| Import theme CSS | Done | `app/assets/tailwind/application.css` |
| Configure theme colors | Done | `theme.css` - generated via `bin/generate-theme --primary '#06b6d4'` |
| Update elements.yml | Done | Synced with new API (color/variant/shape/size) |
| Add FormBuilder | **Pending** | Copy from gem |

---

## 2. Color Philosophy

### 2.1 Core Principle

> **If a color communicates meaning (state, action, feedback), use semantic colors. If it's purely structural or decorative, direct colors are acceptable.**

### 2.2 Semantic Color Mapping

| Semantic | CSS Variable | Tailwind Equivalent | Use For |
|----------|--------------|---------------------|---------|
| `primary` | `--color-primary-*` | cyan | Actions, interactive elements, links, focus states |
| `secondary` | `--color-secondary-*` | purple (offset 60°) | Alternative accent actions |
| `neutral` | `--color-neutral-*` | gray | Neutral actions, less emphasis |
| `success` | `--color-success-*` | emerald | Positive states, confirmations, "active/running" |
| `warning` | `--color-warning-*` | amber | Caution, attention needed, "pending/idle" |
| `danger` | `--color-danger-*` | red | Errors, destructive actions, "failed/error" |

### 2.3 When to Use Semantic Colors

**Always use semantic:**
- Buttons → `color: :primary`, `:neutral`, `:success`, `:warning`, `:danger`
- Interactive text (links, clickable elements) → `text-primary-400`
- Focus rings → `ring-primary-500`, `outline-primary-600`
- Status indicators that map to states:
  - Running/Active/Connected → `text-success-400`, `bg-success-500`
  - Pending/Idle/Waiting → `text-warning-400`, `bg-warning-500`
  - Failed/Error/Disconnected → `text-danger-400`, `bg-danger-500`
- Accent backgrounds for interactive areas → `bg-primary-500/10`

**Keep direct colors:**
- Neutral backgrounds → `bg-zinc-900`, `bg-zinc-800`
- Structural borders → `border-zinc-700`
- Brand-specific elements (GitHub button) → `bg-zinc-100`
- Decorative elements with no semantic meaning

### 2.4 Application-Specific Status Mapping

Based on the connection states in `hubs/show.html.erb`:

| State | Current Color | Semantic Color | Notes |
|-------|---------------|----------------|-------|
| Initializing | `text-zinc-500` | `text-zinc-500` | Neutral, no action needed |
| Connecting | `text-zinc-400` | `text-warning-400` | In progress, attention |
| Connected | `text-emerald-400` | `text-success-400` | Positive outcome |
| E2E Established | `text-emerald-400` | `text-success-400` | Positive outcome |
| Disconnected | `text-zinc-500` | `text-zinc-500` | Neutral state |
| Error/Failed | `text-red-400` | `text-danger-400` | Negative outcome |

### 2.5 elements.yml Status

**Already synced from gem.** The `config/elements.yml` now includes:

- All button sizes with shape variants: `xs`, `sm`, `md`, `lg`, `xl`
- Each size has shapes: `base`, `rounded`, `circular` (circular = icon button with square padding)
- All colors: `primary`, `secondary`, `neutral`, `warning`, `danger`
- Each color has variants: `solid`, `soft`, `outline`, `ghost`

No manual updates needed.

---

## 3. Component APIs

### 3.1 ButtonComponent

All buttons should use ButtonComponent. It accepts all standard Rails helper attributes.

**New API (color/variant/shape/size):**

```ruby
Elements::ButtonComponent.new(
  as: :button,          # Tag: :button, :a, :span, etc.
  color: :primary,      # :primary, :secondary, :neutral, :warning, :danger
  variant: :solid,      # :solid, :soft, :outline, :ghost
  shape: :base,         # :base, :rounded, :circular (circular = icon button)
  size: :md,            # :xs, :sm, :md, :lg, :xl
  # Pass-through attributes:
  href: "/path",        # For links (as: :a)
  data: { action: "controller#method" },  # Stimulus
  disabled: true,
  command: "show-modal",     # Native dialog control
  commandfor: "dialog-id",   # Native dialog control
  class: "w-full",      # Additional classes
  # ... any other HTML attribute
)
```

**Variant Reference:**

| Variant | Background | Border | Text | Use Case |
|---------|------------|--------|------|----------|
| `:solid` | Filled | None | Contrast | Primary actions |
| `:soft` | Tinted | None | Colored | Secondary emphasis |
| `:outline` | Transparent | Colored | Colored | Tertiary actions |
| `:ghost` | Transparent | None | Colored | Icon buttons, minimal UI |

**Examples:**

```erb
<%# Primary button with stimulus action %>
<%= render Elements::ButtonComponent.new(
  color: :primary,
  data: { action: "agents#createAgent" }
) { "New Agent" } %>

<%# Link button %>
<%= render Elements::ButtonComponent.new(
  as: :a,
  href: hubs_path,
  color: :primary,
  size: :lg
) { "View Hubs" } %>

<%# Icon button (circular shape implies square padding) %>
<%= render Elements::ButtonComponent.new(
  color: :primary,
  variant: :ghost,
  shape: :circular,
  data: { action: "agents#createAgent" },
  title: "New Agent"
) do %>
  <svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 4v16m8-8H4" />
  </svg>
<% end %>

<%# Button that opens a dialog (native, no JS) %>
<%= render Elements::ButtonComponent.new(
  color: :primary,
  command: "show-modal",
  commandfor: "new-agent-modal"
) { "New Agent" } %>

<%# Button that closes a dialog (native, no JS) %>
<%= render Elements::ButtonComponent.new(
  color: :neutral,
  variant: :ghost,
  command: "close",
  commandfor: "new-agent-modal"
) { "Cancel" } %>
```

### 3.2 DialogComponent

Native `<dialog>` element. Control via `command`/`commandfor` attributes - avoid JavaScript when possible.

```ruby
Elements::DialogComponent.new(
  id: "dialog-id",      # Required - used for command targeting
  open: false,          # Initially open?
  style: :centered      # :centered or :bottom
)
```

**Opening and closing (prefer native HTML):**

```erb
<%# Button to open - uses native command attribute %>
<button command="show-modal" commandfor="my-dialog">Open</button>

<%# Or with ButtonComponent %>
<%= render Elements::ButtonComponent.new(
  command: "show-modal",
  commandfor: "my-dialog"
) { "Open" } %>

<%# Button to close (inside dialog) %>
<%= render Elements::ButtonComponent.new(
  command: "close",
  commandfor: "my-dialog"
) { "Cancel" } %>
```

**When JavaScript is unavoidable:**

Only use programmatic control when there's no user-initiated trigger (e.g., closing after async operation completes):

```javascript
// Only when necessary - prefer command/commandfor
document.getElementById('my-dialog').showModal();
document.getElementById('my-dialog').close();
```

### 3.3 ToggleComponent

Modern toggle switch. Use directly or via FormBuilder.

```ruby
Elements::ToggleComponent.new(
  name,                 # Required - form field name
  checked: false,       # Initial state
  value: "1",           # Value when checked
  disabled: false       # Disable interaction
)
```

**Direct usage:**
```erb
<%= render Elements::ToggleComponent.new(
  "user[notifications]",
  checked: @user.notifications,
  value: "1"
) %>
```

**With FormBuilder (preferred in forms):**
```erb
<%= form_with model: @user, builder: Elements::FormBuilder do |f| %>
  <%= f.toggle :notifications %>
<% end %>
```

### 3.4 FormBuilder

Provides `elements_select` and `toggle` methods for `form_with`.

**Setup:** Copy `lib/tailwindplus_elements_components/form_builder.rb` to `lib/elements/form_builder.rb` and configure.

**Usage:**
```erb
<%= form_with model: @user, builder: Elements::FormBuilder do |f| %>
  <%# Toggle %>
  <%= f.toggle :email_notifications %>

  <%# Select with choices array %>
  <%= f.elements_select :country, [
    ["United States", "us"],
    ["Canada", "ca"]
  ], { prompt: "Select country" } %>

  <%# Select with block for custom rendering %>
  <%= f.elements_select :category do |select| %>
    <% select.with_menu do %>
      <%= select.option(value: "tech", display: "Technology") %>
      <%= select.option(value: "design", display: "Design") %>
    <% end %>
  <% end %>
<% end %>
```

---

## 4. File-by-File Audit

### 4.1 `app/views/hubs/show.html.erb` (354 lines)

**Priority:** High - Contains modal and most buttons

#### Parent Container (Line 15-23)

**Remove modal outlet reference:**
```erb
<%# Remove this line %>
data-agents-modal-outlet="#new-agent-modal"
```

#### Icon Buttons (Lines 96-120)

**Current:**
```erb
<button type="button"
        data-action="agents#createAgent"
        class="p-1.5 text-cyan-400 hover:bg-zinc-800 rounded transition-colors"
        title="New Agent">
  <svg class="w-4 h-4">...</svg>
</button>
```

**After:**
```erb
<%= render Elements::ButtonComponent.new(
  color: :primary,
  variant: :ghost,
  shape: :circular,
  data: { action: "agents#createAgent" },
  title: "New Agent"
) do %>
  <svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 4v16m8-8H4" />
  </svg>
<% end %>
```

Apply same pattern to Close Agent (line 104) and Refresh (line 113) buttons.

#### Primary "New Agent" Button (Lines 132-139)

**Current:**
```erb
<button type="button"
        data-action="agents#createAgent"
        class="w-full flex items-center justify-center gap-2 px-4 py-2.5 bg-cyan-500 hover:bg-cyan-400 text-zinc-950 text-sm font-medium rounded transition-colors">
  <svg class="w-4 h-4">...</svg>
  New Agent
</button>
```

**After:**
```erb
<%= render Elements::ButtonComponent.new(
  color: :primary,
  class: "w-full flex items-center justify-center gap-2",
  command: "show-modal",
  commandfor: "new-agent-modal"
) do %>
  <svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 4v16m8-8H4" />
  </svg>
  New Agent
<% end %>
```

**Note:** Changed from `data-action="agents#createAgent"` to `command="show-modal"`. The agents controller's `createAgent` method did two things: reset state and show modal. We'll move state reset to a different trigger (see JS changes).

#### Secondary "Close Agent" Button (Lines 140-148)

**After:**
```erb
<%= render Elements::ButtonComponent.new(
  color: :neutral,
  class: "w-full flex items-center justify-center gap-2",
  data: { action: "agents#closeAgent" },
  disabled: true
) do %>
  <svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M6 18L18 6M6 6l12 12" />
  </svg>
  Close Agent
<% end %>
```

#### "Share Hub" Button (Lines 176-184)

**After:**
```erb
<%= render Elements::ButtonComponent.new(
  color: :neutral,
  class: "w-full flex items-center justify-center gap-2",
  data: {
    action: "connection#requestInviteBundle",
    connection_target: "shareBtn"
  }
) do %>
  <svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M8.684 13.342C8.886 12.938 9 12.482 9 12c0-.482-.114-.938-.316-1.342m0 2.684a3 3 0 110-2.684m0 2.684l6.632 3.316m-6.632-6l6.632-3.316m0 0a3 3 0 105.367-2.684 3 3 0 00-5.367 2.684zm0 9.316a3 3 0 105.368 2.684 3 3 0 00-5.368-2.684z" />
  </svg>
  Share Hub
<% end %>
```

#### Mobile Touch Controls (Lines 225-242)

**Keep inline** - These have terminal-specific styling and grid layout. Not a good fit for ButtonComponent abstraction.

#### Modal (Lines 246-352)

**Current:**
```erb
<div id="new-agent-modal"
     data-controller="modal"
     class="hidden fixed inset-0 z-50">
  <div data-action="modal#closeFromBackdrop" ...>
    <div data-modal-target="content" ...>
      <!-- content -->
    </div>
  </div>
</div>
```

**After:**
```erb
<%= render Elements::DialogComponent.new(id: "new-agent-modal") do %>
  <%# Step 1: Worktree Selection %>
  <div data-agents-target="step1" class="flex flex-col flex-1 min-h-0">
    <div class="px-6 py-4 border-b border-zinc-800 shrink-0">
      <div class="flex items-center gap-2 mb-1">
        <span class="w-6 h-6 rounded-full bg-primary-500 text-zinc-950 text-xs font-bold flex items-center justify-center">1</span>
        <h3 class="text-lg font-semibold text-zinc-100">Select Worktree</h3>
      </div>
      <p class="text-sm text-zinc-500 ml-8">Choose an existing worktree or create a new one</p>
    </div>

    <div class="flex-1 overflow-y-auto">
      <%# Existing Worktrees %>
      <div class="p-4 border-b border-zinc-800">
        <h4 class="text-sm font-medium text-zinc-300 mb-3">Existing Worktrees</h4>
        <div data-agents-target="worktreeList" class="space-y-2 max-h-48 overflow-y-auto">
          <div class="text-center py-4 text-zinc-500 text-sm">Loading worktrees...</div>
        </div>
      </div>

      <%# Create New %>
      <div class="p-4">
        <h4 class="text-sm font-medium text-zinc-300 mb-3">Create New</h4>
        <div class="flex gap-2">
          <input type="text"
                 data-agents-target="newBranchInput"
                 placeholder="Issue # or branch name"
                 class="flex-1 px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-lg text-zinc-100 placeholder-zinc-500 text-sm focus:outline-hidden focus:ring-2 focus:ring-primary-500 focus:border-transparent"
                 autocomplete="off"
                 data-action="keydown.enter->agents#selectNewBranch">
          <%= render Elements::ButtonComponent.new(
            color: :primary,
            data: { action: "agents#selectNewBranch" }
          ) { "Next" } %>
        </div>
      </div>
    </div>

    <div class="px-4 py-3 border-t border-zinc-800 shrink-0">
      <%= render Elements::ButtonComponent.new(
        color: :neutral,
        variant: :ghost,
        class: "w-full",
        command: "close",
        commandfor: "new-agent-modal"
      ) { "Cancel" } %>
    </div>
  </div>

  <%# Step 2: Prompt Input %>
  <div data-agents-target="step2" class="hidden flex flex-col flex-1 min-h-0">
    <div class="px-6 py-4 border-b border-zinc-800 shrink-0">
      <div class="flex items-center gap-2 mb-1">
        <span class="w-6 h-6 rounded-full bg-primary-500 text-zinc-950 text-xs font-bold flex items-center justify-center">2</span>
        <h3 class="text-lg font-semibold text-zinc-100">Initial Prompt</h3>
      </div>
      <p class="text-sm text-zinc-500 ml-8">What should the agent work on? (optional)</p>
    </div>

    <div class="flex-1 overflow-y-auto p-4">
      <div class="mb-4 p-3 bg-zinc-800/50 border border-zinc-700 rounded-lg">
        <div class="flex items-center gap-2 text-sm">
          <svg class="w-4 h-4 text-success-400 shrink-0" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M3 7v10a2 2 0 002 2h14a2 2 0 002-2V9a2 2 0 00-2-2h-6l-2-2H5a2 2 0 00-2 2z"></path>
          </svg>
          <span class="text-zinc-400">Worktree:</span>
          <span data-agents-target="selectedWorktreeLabel" class="text-zinc-200 font-mono truncate"></span>
        </div>
      </div>

      <label class="block text-sm font-medium text-zinc-300 mb-2">Task Description</label>
      <textarea data-agents-target="promptInput"
                rows="4"
                placeholder="Describe what you want the agent to work on... (leave blank for interactive session)"
                class="w-full px-3 py-2 bg-zinc-800 border border-zinc-700 rounded-lg text-zinc-100 placeholder-zinc-500 text-sm focus:outline-hidden focus:ring-2 focus:ring-primary-500 focus:border-transparent resize-none"></textarea>
      <p class="text-xs text-zinc-500 mt-2">Leave blank to start an interactive session</p>
    </div>

    <div class="px-4 py-3 border-t border-zinc-800 shrink-0 flex gap-2">
      <%= render Elements::ButtonComponent.new(
        color: :neutral,
        variant: :ghost,
        class: "flex-1",
        data: { action: "agents#goBackToStep1" }
      ) { "Back" } %>
      <%= render Elements::ButtonComponent.new(
        color: :primary,
        class: "flex-1",
        data: { action: "agents#submitAgent" }
      ) { "Start Agent" } %>
    </div>
  </div>
<% end %>
```

#### Color Updates in Status Elements

**Connection status icon (line 48):** Keep `text-zinc-500` - neutral loading state

**Security banner text (line 78):** Keep `text-zinc-400` - informational

**E2E Ready badge (line 165):**
```erb
<%# Change from %>
class="bg-emerald-500/10 text-emerald-400"
<%# To %>
class="bg-success-500/10 text-success-400"
```

---

### 4.2 `app/views/home/index.html.erb` (164 lines)

**Priority:** Medium

#### Copy Button (Lines 29-37)

**Keep inline** - Has clipboard controller targets with custom styling.

#### "View Active Hubs" Link (Lines 84-89)

**After:**
```erb
<%= render Elements::ButtonComponent.new(
  as: :a,
  href: hubs_path,
  color: :primary,
  size: :lg,
  class: "inline-flex items-center gap-2"
) do %>
  <svg class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M9.75 17L9 20l-1 1h8l-1-1-.75-3M3 13h18M5 17h14a2 2 0 002-2V5a2 2 0 00-2-2H5a2 2 0 00-2 2v10a2 2 0 002 2z" />
  </svg>
  View Active Hubs
<% end %>
```

#### "Connect New Hub" Link (Lines 91-96)

**After:**
```erb
<%= render Elements::ButtonComponent.new(
  as: :a,
  href: new_users_hub_path,
  color: :neutral,
  size: :lg,
  class: "inline-flex items-center gap-2"
) do %>
  <svg class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
    <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 4v16m8-8H4" />
  </svg>
  Connect New Hub
<% end %>
```

#### "Sign in with GitHub" Link (Lines 120-127)

**Keep inline** - Brand-specific styling with light background.

#### Feature Card Icons (Lines 133-160)

**Change accent color to semantic:**
```erb
<%# Change from %>
class="w-10 h-10 bg-cyan-500/10 rounded-lg"
class="w-5 h-5 text-cyan-400"

<%# To %>
class="w-10 h-10 bg-primary-500/10 rounded-lg"
class="w-5 h-5 text-primary-400"
```

---

### 4.4 `app/views/users/hubs/new.html.erb` (60 lines)

#### Submit Button (Lines 46-49)

**After:**
```erb
<%= render Elements::ButtonComponent.new(
  as: :button,
  type: :submit,
  color: :primary,
  size: :lg,
  class: "w-full"
) { "Continue" } %>
```

#### Icon Container (Line 5)

**Change to semantic:**
```erb
<%# From %>
class="bg-cyan-500/10"
class="text-cyan-400"

<%# To %>
class="bg-primary-500/10"
class="text-primary-400"
```

---

### 4.5 `app/views/users/hubs/confirm.html.erb` (69 lines)

#### Cancel Button (Lines 52-57)

**After:**
```erb
<%= render Elements::ButtonComponent.new(
  as: :a,
  href: new_users_hub_path,
  color: :neutral,
  size: :lg,
  class: "flex-1 w-full text-center"
) { "Cancel" } %>
```

#### Approve Button (Lines 62-65)

**After:**
```erb
<%= render Elements::ButtonComponent.new(
  as: :button,
  type: :submit,
  color: :success,
  size: :lg,
  class: "w-full"
) { "Approve" } %>
```

---

## 5. JavaScript Changes

### 5.1 Delete `app/javascript/controllers/modal_controller.js`

Entire file (76 lines) replaced by DialogComponent with native `command`/`commandfor`.

### 5.2 Update `app/javascript/controllers/agents_controller.js`

#### Remove modal outlet (Line 28)

```javascript
// Before
static outlets = ["connection", "modal"];

// After
static outlets = ["connection"];
```

#### Update `showCreatingState()` (Lines 171-174)

The modal now closes via `command="close"` on the Start Agent button, but we still need to close it programmatically when agent creation starts from worktree selection:

```javascript
// Before
if (this.hasModalOutlet) {
  this.modalOutlet.hide();
}

// After - only when JS control is unavoidable
const dialog = document.getElementById('new-agent-modal');
if (dialog?.open) {
  dialog.close();
}
```

#### Update `submitAgent()` (Lines 463-468)

```javascript
// Before
this.resetModalState();
if (this.hasModalOutlet) {
  this.modalOutlet.hide();
}

// After - dialog closes via command="close" on button, just reset state
this.resetModalState();
// Dialog closes via native command attribute on "Start Agent" button
```

**Wait** - the "Start Agent" button uses `data-action="agents#submitAgent"`, not `command="close"`. We need to close the dialog after the action runs. Options:

1. Add `command="close"` AND `data-action` (both will fire)
2. Close programmatically in `submitAgent()`

**Recommendation:** Use option 1 - add both attributes to the button:

```erb
<%= render Elements::ButtonComponent.new(
  color: :primary,
  class: "flex-1",
  data: { action: "agents#submitAgent" },
  command: "close",
  commandfor: "new-agent-modal"
) { "Start Agent" } %>
```

This way the dialog closes natively and the action runs. Update `submitAgent()` to just handle the action:

```javascript
submitAgent() {
  if (!this.pendingSelection || !this.connection) return;

  const prompt = this.hasPromptInputTarget ? this.promptInputTarget.value?.trim() : "";

  if (this.pendingSelection.type === "existing") {
    this.connection.send("reopen_worktree", {
      path: this.pendingSelection.path,
      branch: this.pendingSelection.branch,
      prompt: prompt || null,
    });
  } else {
    this.connection.send("create_agent", {
      issue_or_branch: this.pendingSelection.issueOrBranch,
      prompt: prompt || null,
    });
  }

  // Reset state - dialog closes via native command attribute
  this.resetModalState();
}
```

#### Remove `createAgent()` method (Lines 496-510)

The "New Agent" button now uses `command="show-modal"` directly. But we still need to:
1. Reset modal state when opening
2. Refresh worktree list

**Option A:** Keep a simpler version that's called via `data-action` alongside `command`:

```javascript
createAgent() {
  this.resetModalState();
  if (this.connection) {
    this.connection.send("list_worktrees");
  }
  this.updateWorktreeList();
  // Dialog opens via native command attribute
}
```

Button:
```erb
<%= render Elements::ButtonComponent.new(
  color: :primary,
  class: "w-full",
  data: { action: "agents#createAgent" },
  command: "show-modal",
  commandfor: "new-agent-modal"
) { "New Agent" } %>
```

**Option B:** Use dialog events to trigger setup:

```javascript
connect() {
  // ... existing code ...

  // Setup modal when it opens
  const dialog = document.getElementById('new-agent-modal');
  dialog?.addEventListener('open', () => this.prepareModal());
}

prepareModal() {
  this.resetModalState();
  if (this.connection) {
    this.connection.send("list_worktrees");
  }
  this.updateWorktreeList();
}
```

**Recommendation:** Option A is simpler and more explicit.

---

## 6. New Files to Add

### 6.1 FormBuilder

Copy from elements gem and adapt:

**File:** `lib/elements/form_builder.rb`

```ruby
module Elements
  class FormBuilder < ActionView::Helpers::FormBuilder
    def elements_select(method, choices = nil, options = {}, html_options = {}, &block)
      selected_value = @object&.public_send(method)

      field_options = {
        name: field_name(method),
        id: field_id(method),
        value: selected_value || ""
      }

      field_options[:prompt] = options[:prompt] if options[:prompt]
      field_options[:include_blank] = options[:include_blank] if options.key?(:include_blank)
      field_options[:required] = options[:required] if options[:required]
      field_options.merge!(html_options)

      if choices.present? && block.nil?
        @template.render ::Elements::SelectComponent.new(**field_options) do |select|
          select.with_menu do
            choices.map do |choice|
              case choice
              when Array
                display, value = choice
                select.option(value: value, display: display)
              when Hash
                select.option(value: choice[:value], display: choice[:text] || choice[:display])
              else
                select.option(value: choice)
              end
            end.join.html_safe
          end
        end
      else
        @template.render ::Elements::SelectComponent.new(**field_options), &block
      end
    end

    def toggle(method, options = {}, checked_value = "1", unchecked_value = "0")
      current_value = @object&.public_send(method)

      is_checked = case current_value
                   when checked_value, true, "true", 1, "1" then true
                   else false
                   end

      field_options = {
        name: field_name(method),
        id: field_id(method),
        value: checked_value,
        checked: is_checked
      }

      hidden_field_tag = @template.tag.input(
        type: "hidden",
        name: field_name(method),
        value: unchecked_value,
        autocomplete: "off"
      )

      field_options.merge!(options)

      hidden_field_tag + @template.render(::Elements::ToggleComponent.new(field_name(method), **field_options))
    end

    alias_method :element_select, :elements_select
  end
end
```

**File:** `config/initializers/elements_form_builder.rb`

```ruby
require_relative "../../lib/elements/form_builder"
```

### 6.2 elements.yml

**No action needed.** Already synced from gem with all required styles:
- Sizes: `xs`, `sm`, `md`, `lg`, `xl` with shapes `base`, `rounded`, `circular`
- Colors: `primary`, `secondary`, `neutral`, `warning`, `danger` with variants `solid`, `soft`, `outline`, `ghost`

---

## 7. Testing Checklist

### 7.1 DialogComponent (Native Control)

- [ ] "New Agent" button opens dialog (via `command="show-modal"`)
- [ ] "Cancel" button closes dialog (via `command="close"`)
- [ ] Clicking backdrop closes dialog
- [ ] Pressing Escape closes dialog
- [ ] "Start Agent" submits action AND closes dialog
- [ ] Two-step flow works
- [ ] Worktree list populates on open
- [ ] State resets when reopening

### 7.2 ButtonComponent

- [ ] Primary buttons (cyan)
- [ ] Secondary buttons (outline/ghost)
- [ ] Success buttons (emerald)
- [ ] Icon buttons with ghost styling
- [ ] Link buttons navigate correctly
- [ ] Disabled states
- [ ] All data attributes pass through
- [ ] command/commandfor attributes work

### 7.3 ToggleComponent

- [ ] Renders as switch (not checkbox)
- [ ] Reflects current value on load
- [ ] Changes state on click
- [ ] Form submission includes value
- [ ] Hidden field for unchecked value

### 7.4 FormBuilder

- [ ] `f.toggle` works in forms
- [ ] `f.elements_select` works with choices array

### 7.5 Semantic Colors

- [ ] `primary-*` renders as cyan
- [ ] `success-*` renders as emerald
- [ ] `warning-*` renders as amber
- [ ] `danger-*` renders as red
- [ ] Interactive text uses `text-primary-*`
- [ ] Status indicators use appropriate semantic colors

### 7.6 No Regressions

- [ ] No JavaScript console errors
- [ ] Clipboard copy still works
- [ ] Terminal display still works
- [ ] Agent creation flow works end-to-end
- [ ] Connection states display correctly

---

## 8. Migration Order

1. **Add FormBuilder** - Create lib/elements/form_builder.rb + initializer
2. **Migrate hubs/show.html.erb** - Biggest file, modal + buttons
3. **Update agents_controller.js** - Remove modal outlet, adjust methods
4. **Delete modal_controller.js** - Cleanup
5. **Migrate settings/show.html.erb** - Toggle + button
6. **Migrate home/index.html.erb** - Link buttons
7. **Migrate users/hubs views** - Small changes
8. **Update semantic colors throughout** - text-cyan → text-primary, etc.
9. **Test everything**

---

## 9. Files Summary

| File | Action | Changes |
|------|--------|---------|
| `config/elements.yml` | Done | Synced from gem |
| `lib/elements/form_builder.rb` | Create | FormBuilder for toggle/select |
| `config/initializers/elements_form_builder.rb` | Create | Require form builder |
| `app/views/hubs/show.html.erb` | Edit | Dialog, buttons, semantic colors |
| `app/views/settings/show.html.erb` | Edit | Toggle via FormBuilder, button |
| `app/views/home/index.html.erb` | Edit | Link buttons, semantic colors |
| `app/views/users/hubs/new.html.erb` | Edit | Button, semantic colors |
| `app/views/users/hubs/confirm.html.erb` | Edit | Cancel/Approve buttons |
| `app/javascript/controllers/agents_controller.js` | Edit | Remove modal outlet |
| `app/javascript/controllers/modal_controller.js` | Delete | Replaced by DialogComponent |
