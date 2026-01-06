---
name: rails-frontend-guidelines
description: Rails frontend development guidelines using Hotwire (Turbo + Stimulus), Tailwind CSS, and ViewComponent. Modern patterns for server-rendered HTML with progressive enhancement, zero-build frontend architecture, and Rails conventions. Use when creating views, components, Stimulus controllers, partials, or working with frontend code.
---

# Rails Frontend Guidelines

## Critical Rules

**NEVER do these things:**

1. **NEVER use `<style>` tags** - Use Tailwind utility classes exclusively. No inline CSS, no `<style>` blocks, no custom CSS files.

2. **NEVER run `bin/rails assets:precompile` in development** - Creates cached assets that override live code. If you accidentally run it: `bin/rails assets:clobber` and restart.

3. **NEVER write custom CSS** - Tailwind only. No exceptions.

4. **Avoid arbitrary values** - `w-[123px]` is a last resort. Prefer standard Tailwind utilities. Arbitrary values defeat the design system.

5. **Single responsibility controllers** - Each Stimulus controller does ONE thing. A WebRTC controller shouldn't handle modals. A form controller shouldn't handle dropdowns. Extract separate controllers.

6. **Use implicit actions** - Omit the event when it's the default:
   ```html
   <%# Good - implicit click %>
   <button data-action="modal#open">Open</button>

   <%# Bad - redundant click %>
   <button data-action="click->modal#open">Open</button>
   ```
   Defaults: button=click, form=submit, input=input, select=change

---

## Project Patterns

### Stimulus Controller Template

```javascript
import { Controller } from "@hotwired/stimulus"

export default class extends Controller {
  static targets = ["output"]
  static values = { url: String }

  connect() {
    // Setup
  }

  disconnect() {
    // Cleanup
  }

  toggle() {
    this.outputTarget.classList.toggle("hidden")
  }
}
```

### Turbo Frame Pattern

```erb
<%# Wrap interactive sections in frames %>
<%= turbo_frame_tag dom_id(@post) do %>
  <%= render @post %>
<% end %>

<%# Lazy load %>
<%= turbo_frame_tag "comments", src: post_comments_path(@post), loading: :lazy do %>
  <p>Loading...</p>
<% end %>
```

### Turbo Stream Response

```ruby
# Controller
def create
  @comment = @post.comments.create!(comment_params)
  respond_to do |format|
    format.turbo_stream
    format.html { redirect_to @post }
  end
end
```

```erb
<%# create.turbo_stream.erb %>
<%= turbo_stream.append "comments", @comment %>
```

---

## Resources

- [Turbo Guide](resources/turbo-guide.md) - Frames, Streams, Drive
- [Stimulus Guide](resources/stimulus-guide.md) - Controllers, targets, values
- [Complete Examples](resources/complete-examples.md) - Full working patterns
