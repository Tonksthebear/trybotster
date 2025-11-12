---
name: rails-frontend-guidelines
description: Rails frontend development guidelines using Hotwire (Turbo + Stimulus), Tailwind CSS, and ViewComponent. Modern patterns for server-rendered HTML with progressive enhancement, zero-build frontend architecture, and Rails conventions. Use when creating views, components, Stimulus controllers, partials, or working with frontend code.
---

# Rails Frontend Development Guidelines

## Purpose

Comprehensive guide for modern Rails frontend development using Hotwire (Turbo + Stimulus), Tailwind CSS, and server-side rendering. Emphasizes progressive enhancement, minimal JavaScript, and Rails conventions.

## When to Use This Skill

- Creating new views or partials
- Building Stimulus controllers
- Working with Turbo Frames or Turbo Streams
- Styling with Tailwind CSS
- Creating ViewComponents
- Adding frontend interactivity
- Organizing frontend code
- Performance optimization

---

## Quick Start

### New View Checklist

Creating a view? Follow this checklist:

- [ ] Use semantic HTML5 elements
- [ ] Turbo Frame for interactive sections: `<turbo-frame id="unique-id">`
- [ ] Turbo Stream for real-time updates
- [ ] Tailwind utility classes for styling
- [ ] Minimal Stimulus controllers for interactivity
- [ ] Partials for reusable components
- [ ] Accessible markup (ARIA labels, semantic elements)
- [ ] Mobile-first responsive design
- [ ] Progressive enhancement (works without JS)

### New Stimulus Controller Checklist

Creating a Stimulus controller? Follow this:

- [ ] Name matches HTML: `data-controller="hello"` → `hello_controller.js`
- [ ] Use targets for DOM elements: `data-hello-target="output"`
- [ ] Use values for configuration: `data-hello-name-value="World"`
- [ ] Use actions for events: `data-action="click->hello#greet"`
- [ ] Keep controllers small and focused (single responsibility)
- [ ] Clean up in `disconnect()` if needed
- [ ] Use classes for CSS manipulation: `data-hello-class="hidden"`

---

## File Structure Quick Reference

```
app/
  views/
    layouts/
      application.html.erb          # Main layout
    shared/
      _nav.html.erb                 # Shared partials
    posts/
      index.html.erb                # Index view
      show.html.erb                 # Show view
      _post.html.erb                # Partial
      _form.html.erb                # Form partial

  javascript/
    controllers/
      application.js                # Stimulus application
      index.js                      # Controller index
      hello_controller.js           # Stimulus controllers
      form_controller.js
      dropdown_controller.js

  components/                       # ViewComponents (optional)
    button_component.rb
    button_component.html.erb
```

---

## Topic Guides

### 🎨 View & Partial Patterns

**Rails View Conventions:**

- Use ERB templates (`.html.erb`)
- Partials start with underscore: `_post.html.erb`
- Render partials: `<%= render 'posts/post', post: @post %>`
- Collections: `<%= render @posts %>` (auto-finds `_post.html.erb`)

**Key Concepts:**

- Semantic HTML first (progressive enhancement)
- Tailwind for styling (utility-first CSS)
- Turbo Frames for page sections
- Partials for reusable components
- ViewComponents for complex components

**[📖 Complete Guide: resources/view-patterns.md](resources/view-patterns.md)**

---

### ⚡ Turbo (Hotwire)

**Turbo Drive:**

- Automatic page navigation without full reload
- Enabled by default in Rails 7+
- Use `data-turbo="false"` to disable on specific links/forms

**Turbo Frames:**

- Scoped page updates: `<turbo-frame id="post_123">`
- Lazy loading: `<turbo-frame src="/posts/123" loading="lazy">`
- Break out: `data-turbo-frame="_top"`

**Turbo Streams:**

- Real-time updates over WebSocket or HTTP
- 7 actions: append, prepend, replace, update, remove, before, after
- Format: `respond_to { |format| format.turbo_stream }`

**[📖 Complete Guide: resources/turbo-guide.md](resources/turbo-guide.md)**

---

### 🎮 Stimulus Controllers

**Stimulus = "Modest JavaScript Framework"**

- Sprinkles JavaScript on server-rendered HTML
- Three core concepts: Controllers, Actions, Targets
- Lifecycle: `connect()` → `disconnect()`
- Values for configuration (automatically typed)

**Controller Structure:**

```javascript
import { Controller } from "@hotwired/stimulus";

export default class extends Controller {
  static targets = ["output", "input"];
  static values = { url: String, count: Number };
  static classes = ["hidden", "active"];

  connect() {
    // Called when element appears
  }

  disconnect() {
    // Cleanup (remove listeners, etc.)
  }
}
```

**[📖 Complete Guide: resources/stimulus-guide.md](resources/stimulus-guide.md)**

---

### 🎨 Styling with Tailwind

**Tailwind Utility-First:**

- Apply utilities directly in HTML
- Responsive: `md:`, `lg:`, `xl:` prefixes
- Hover/focus: `hover:`, `focus:` prefixes
- Dark mode: `dark:` prefix (if configured)

**Common Patterns:**

```erb
<!-- Card -->
<div class="bg-white rounded-lg shadow-md p-6">
  <h2 class="text-xl font-bold mb-4">Title</h2>
  <p class="text-gray-700">Content</p>
</div>

<!-- Button -->
<button class="bg-blue-500 hover:bg-blue-700 text-white font-bold py-2 px-4 rounded">
  Click me
</button>

<!-- Responsive Grid -->
<div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
  <!-- Items -->
</div>
```

**[📖 Complete Guide: resources/tailwind-guide.md](resources/tailwind-guide.md)**

---

### 📁 File Organization

**Views Organization:**

- Group by resource: `app/views/posts/`
- Shared partials: `app/views/shared/`
- Layouts: `app/views/layouts/`
- Mailer views: `app/views/user_mailer/`

**JavaScript Organization:**

- Stimulus controllers: `app/javascript/controllers/`
- Helpers/utilities: `app/javascript/helpers/`
- Importmap for dependencies (no build step)

**Component Organization:**

- ViewComponents: `app/components/`
- Component templates: `app/components/{name}_component.html.erb`
- Component Ruby class: `app/components/{name}_component.rb`

**[📖 Complete Guide: resources/file-organization.md](resources/file-organization.md)**

---

### 📝 Forms & Validation

**Rails Form Helpers:**

- `form_with model: @post` for model forms
- Automatic CSRF tokens
- Turbo-enabled by default
- Client-side validation with HTML5
- Server-side validation with ActiveModel

**Stimulus Form Enhancement:**

- Auto-save with `change` action
- Character counters
- Dynamic field addition/removal
- Real-time validation feedback

**[📖 Complete Guide: resources/forms-guide.md](resources/forms-guide.md)**

---

### ⏳ Loading & Error States

**Loading States:**

- Turbo progress bar (automatic)
- Skeleton screens with Tailwind
- Loading spinners with Stimulus
- Optimistic UI updates

**Error Handling:**

- Flash messages for user feedback
- Inline form errors with Rails helpers
- Turbo Stream error responses
- Stimulus for dynamic error display

**[📖 Complete Guide: resources/loading-and-error-states.md](resources/loading-and-error-states.md)**

---

### ⚡ Performance

**Optimization Patterns:**

- Turbo Drive for instant navigation
- Lazy loading with Turbo Frames
- Fragment caching in views
- Russian Doll caching
- Database eager loading (N+1 prevention)
- Importmap preloading
- Tailwind JIT mode (production)

**[📖 Complete Guide: resources/performance.md](resources/performance.md)**

---

### 🧩 ViewComponents

**Component-Based Architecture:**

- Ruby classes for component logic
- ERB templates for markup
- Testable in isolation
- Encapsulated styling

**When to Use:**

- Complex reusable components
- Components with logic
- Design system components
- Shared UI patterns

**[📖 Complete Guide: resources/viewcomponent-guide.md](resources/viewcomponent-guide.md)**

---

### 📚 Complete Examples

**Full working examples:**

- Modern view with Turbo Frames
- Complete Stimulus controller
- Form with real-time validation
- Turbo Stream updates
- ViewComponent with Tailwind
- Responsive layouts

**[📖 Complete Guide: resources/complete-examples.md](resources/complete-examples.md)**

---

## Navigation Guide

| Need to...                 | Read this resource                                                   |
| -------------------------- | -------------------------------------------------------------------- |
| Create a view/partial      | [view-patterns.md](resources/view-patterns.md)                       |
| Use Turbo Frames/Streams   | [turbo-guide.md](resources/turbo-guide.md)                           |
| Create Stimulus controller | [stimulus-guide.md](resources/stimulus-guide.md)                     |
| Style with Tailwind        | [tailwind-guide.md](resources/tailwind-guide.md)                     |
| Organize files             | [file-organization.md](resources/file-organization.md)               |
| Build forms                | [forms-guide.md](resources/forms-guide.md)                           |
| Handle loading/errors      | [loading-and-error-states.md](resources/loading-and-error-states.md) |
| Optimize performance       | [performance.md](resources/performance.md)                           |
| Create ViewComponents      | [viewcomponent-guide.md](resources/viewcomponent-guide.md)           |
| See full examples          | [complete-examples.md](resources/complete-examples.md)               |

---

## Core Principles

1. **Server-Side First**: Render HTML on server, enhance with JavaScript
2. **Progressive Enhancement**: Works without JavaScript, better with it
3. **Turbo for Navigation**: Use Turbo Drive/Frames instead of full page reloads
4. **Stimulus for Interactivity**: Minimal JavaScript, attached to HTML
5. **Tailwind for Styling**: Utility-first CSS in templates
6. **Semantic HTML**: Use proper HTML5 elements
7. **Partials for Reuse**: Extract common patterns to partials
8. **ViewComponents for Complexity**: Use for complex reusable components

---

## Quick Reference: Stimulus Syntax

```erb
<!-- Controller -->
<div data-controller="dropdown">

  <!-- Target -->
  <button data-dropdown-target="button"
          data-action="click->dropdown#toggle">
    Toggle
  </button>

  <!-- Value -->
  <div data-dropdown-open-value="false">

    <!-- Class -->
    <ul data-dropdown-target="menu"
        data-dropdown-class="hidden">
      <li>Item 1</li>
      <li>Item 2</li>
    </ul>
  </div>
</div>
```

---

## Modern View Template (Quick Copy)

```erb
<%# app/views/posts/show.html.erb %>

<turbo-frame id="post_<%= @post.id %>">
  <div class="max-w-4xl mx-auto p-6">
    <div class="bg-white rounded-lg shadow-md p-6">
      <h1 class="text-3xl font-bold mb-4">
        <%= @post.title %>
      </h1>

      <div class="prose max-w-none mb-6">
        <%= @post.content %>
      </div>

      <div class="flex gap-4">
        <%= link_to "Edit",
                    edit_post_path(@post),
                    class: "bg-blue-500 hover:bg-blue-700 text-white font-bold py-2 px-4 rounded" %>

        <%= button_to "Delete",
                      post_path(@post),
                      method: :delete,
                      data: { turbo_confirm: "Are you sure?" },
                      class: "bg-red-500 hover:bg-red-700 text-white font-bold py-2 px-4 rounded" %>
      </div>
    </div>
  </div>
</turbo-frame>
```

---

## Modern Stimulus Controller Template (Quick Copy)

```javascript
// app/javascript/controllers/hello_controller.js
import { Controller } from "@hotwired/stimulus";

export default class extends Controller {
  static targets = ["name", "output"];
  static values = {
    greeting: { type: String, default: "Hello" },
  };

  connect() {
    console.log("Hello controller connected");
  }

  greet() {
    const name = this.nameTarget.value || "World";
    this.outputTarget.textContent = `${this.greetingValue}, ${name}!`;
  }

  disconnect() {
    // Cleanup if needed
  }
}
```

For complete examples, see [resources/complete-examples.md](resources/complete-examples.md)

---

## Related Skills

- **rails-backend-guidelines**: Backend patterns for controllers and models
- **skill-developer**: For creating new skills

---

**Skill Status**: Adapted for Rails 7+, Hotwire (Turbo + Stimulus), and Tailwind CSS
