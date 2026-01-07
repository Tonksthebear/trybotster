---
name: rails-frontend-guidelines
description: Rails frontend development guidelines using Hotwire (Turbo + Stimulus), Tailwind CSS, and ViewComponent. Combines bold design thinking with modern patterns for server-rendered HTML with progressive enhancement. Use when creating views, components, Stimulus controllers, partials, or working with frontend code.
---

# Rails Frontend Guidelines

## Design Thinking (before coding)

Before writing any Tailwind classes, commit to a **BOLD aesthetic direction**:

### Key Questions
- **Purpose**: What problem does this interface solve? Who uses it?
- **Tone**: Pick a direction and commit fully:
  - Brutally minimal
  - Maximalist chaos
  - Retro-futuristic
  - Editorial/magazine
  - Luxury/refined
  - Playful/toy-like
  - Industrial/utilitarian
  - Soft/pastel
- **Differentiation**: What's the one thing someone will remember?

Bold maximalism and refined minimalism both work—the key is **intentionality, not intensity**.

### Typography
- **Avoid generic fonts**: Inter, Roboto, Arial, system fonts are forgettable
- Pair a distinctive display font with a refined body font
- Unexpected, characterful font choices elevate everything

### Color & Theme
- **Dominant colors with sharp accents** outperform timid, evenly-distributed palettes
- Commit to a cohesive aesthetic—use Tailwind's color system consistently
- Light vs dark: make a choice, execute it fully

### Spatial Composition
- Unexpected layouts, asymmetry, overlap
- Generous negative space OR controlled density (pick one)
- Grid-breaking elements where they serve the design

### Motion
- Focus on **high-impact moments**: one well-orchestrated page load with staggered reveals creates more delight than scattered micro-interactions
- Scroll-triggering and hover states that surprise
- CSS transitions via Tailwind (`transition-all`, `duration-300`, etc.)

### Visual Depth
- Create atmosphere rather than defaulting to solid colors
- Gradient meshes, noise textures, layered transparencies
- Dramatic shadows, decorative borders

### Anti-Patterns (never do these)
- Purple gradients on white backgrounds (cliché AI aesthetic)
- Cookie-cutter layouts and predictable component patterns
- Converging on common choices—each design should be unique
- Generic styling that lacks context-specific character

---

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
