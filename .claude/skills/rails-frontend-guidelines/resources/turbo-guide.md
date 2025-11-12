# Turbo Guide (Hotwire)

Turbo is part of Hotwire and provides three main tools: Turbo Drive, Turbo Frames, and Turbo Streams.

## Turbo Drive

**What it does:** Automatically intercepts link clicks and form submissions, replacing page content without full reload.

**Enabled by default** in Rails 7+. No configuration needed!

### Disabling Turbo Drive

For specific links/forms that need full page reload:

```erb
<%# Disable on a link %>
<%= link_to "Full Reload", some_path, data: { turbo: false } %>

<%# Disable on a form %>
<%= form_with model: @post, data: { turbo: false } do |f| %>
  ...
<% end %>

<%# Disable for entire page (in head) %>
<meta name="turbo-visit-control" content="reload">
```

### Turbo Drive Events

Listen for page changes in Stimulus:

```javascript
// app/javascript/controllers/page_controller.js
import { Controller } from "@hotwired/stimulus"

export default class extends Controller {
  connect() {
    document.addEventListener("turbo:load", this.onPageLoad)
    document.addEventListener("turbo:before-visit", this.beforeVisit)
  }

  disconnect() {
    document.removeEventListener("turbo:load", this.onPageLoad)
    document.removeEventListener("turbo:before-visit", this.beforeVisit)
  }

  onPageLoad = () => {
    console.log("Page loaded via Turbo")
  }

  beforeVisit = (event) => {
    // Can prevent navigation: event.preventDefault()
  }
}
```

---

## Turbo Frames

**What it does:** Updates only a specific part of the page instead of the whole page.

### Basic Turbo Frame

```erb
<%# app/views/posts/show.html.erb %>
<turbo-frame id="post_<%= @post.id %>">
  <h1><%= @post.title %></h1>
  <p><%= @post.content %></p>
  
  <%= link_to "Edit", edit_post_path(@post) %>
</turbo-frame>

<%# app/views/posts/edit.html.erb %>
<turbo-frame id="post_<%= @post.id %>">
  <%= form_with model: @post do |f| %>
    <%= f.text_field :title %>
    <%= f.text_area :content %>
    <%= f.submit %>
  <% end %>
</turbo-frame>
```

**How it works:** Clicking "Edit" only replaces content inside the frame, not the whole page.

### Lazy Loading Frames

Load content on-demand:

```erb
<%# Loads immediately %>
<turbo-frame id="eager_comments" src="<%= post_comments_path(@post) %>">
  Loading comments...
</turbo-frame>

<%# Loads when scrolled into view %>
<turbo-frame id="lazy_related" 
             src="<%= related_posts_path(@post) %>" 
             loading="lazy">
  Loading related posts...
</turbo-frame>
```

### Breaking Out of Frames

Navigate the full page from within a frame:

```erb
<turbo-frame id="modal">
  <%= link_to "Full Page", some_path, data: { turbo_frame: "_top" } %>
  
  <%# Or target a different frame %>
  <%= link_to "Other Frame", some_path, data: { turbo_frame: "other_frame" } %>
</turbo-frame>
```

### Nested Frames

Frames can be nested:

```erb
<turbo-frame id="post_<%= @post.id %>">
  <h1><%= @post.title %></h1>
  
  <turbo-frame id="post_<%= @post.id %>_comments">
    <%= render @post.comments %>
  </turbo-frame>
</turbo-frame>
```

---

## Turbo Streams

**What it does:** Sends multiple updates to the page in one response (perfect for real-time updates).

### 7 Turbo Stream Actions

1. **append** - Add to end of target
2. **prepend** - Add to beginning of target
3. **replace** - Replace entire target
4. **update** - Replace target's content (keeps target element)
5. **remove** - Remove target
6. **before** - Insert before target
7. **after** - Insert after target

### Controller Response

```ruby
# app/controllers/posts_controller.rb
class PostsController < ApplicationController
  def create
    @post = Post.new(post_params)
    
    respond_to do |format|
      if @post.save
        format.turbo_stream do
          render turbo_stream: turbo_stream.prepend("posts", partial: "posts/post", locals: { post: @post })
        end
        format.html { redirect_to @post }
      else
        format.html { render :new, status: :unprocessable_entity }
      end
    end
  end

  def destroy
    @post = Post.find(params[:id])
    @post.destroy
    
    respond_to do |format|
      format.turbo_stream do
        render turbo_stream: turbo_stream.remove(@post)
      end
      format.html { redirect_to posts_path }
    end
  end
end
```

### Turbo Stream Template

Create a `.turbo_stream.erb` file:

```erb
<%# app/views/posts/create.turbo_stream.erb %>
<%= turbo_stream.prepend "posts", partial: "posts/post", locals: { post: @post } %>
<%= turbo_stream.update "post_form", partial: "posts/form", locals: { post: Post.new } %>
<%= turbo_stream.update "flash", partial: "shared/flash", locals: { notice: "Post created!" } %>
```

### Multiple Stream Actions

```ruby
# In controller
format.turbo_stream do
  render turbo_stream: [
    turbo_stream.prepend("posts", partial: "posts/post", locals: { post: @post }),
    turbo_stream.update("post_count", html: Post.count),
    turbo_stream.remove("new_post_form")
  ]
end
```

### Broadcast Updates (Real-time)

For WebSocket updates across users:

```ruby
# app/models/post.rb
class Post < ApplicationRecord
  after_create_commit { broadcast_prepend_to "posts" }
  after_update_commit { broadcast_replace_to "posts" }
  after_destroy_commit { broadcast_remove_to "posts" }
end
```

Then in your view:

```erb
<%# app/views/posts/index.html.erb %>
<%= turbo_stream_from "posts" %>

<div id="posts">
  <%= render @posts %>
</div>
```

### Custom Turbo Stream Actions

You can create custom actions with Stimulus:

```javascript
// app/javascript/controllers/turbo_streams_controller.js
import { Controller } from "@hotwired/stimulus"
import { StreamActions } from "@hotwired/turbo"

export default class extends Controller {
  connect() {
    StreamActions.console_log = function() {
      console.log(this.getAttribute("message"))
    }
  }
}
```

---

## Common Patterns

### Inline Editing

```erb
<%# app/views/posts/_post.html.erb %>
<turbo-frame id="<%= dom_id(post) %>">
  <div class="bg-white p-4 rounded shadow">
    <h2 class="text-xl font-bold"><%= post.title %></h2>
    <p><%= post.content %></p>
    <%= link_to "Edit", edit_post_path(post), class: "text-blue-500" %>
  </div>
</turbo-frame>

<%# app/views/posts/edit.html.erb %>
<turbo-frame id="<%= dom_id(@post) %>">
  <%= form_with model: @post do |f| %>
    <%= f.text_field :title, class: "border rounded p-2 w-full" %>
    <%= f.text_area :content, class: "border rounded p-2 w-full mt-2" %>
    <div class="mt-2">
      <%= f.submit "Save", class: "bg-blue-500 text-white px-4 py-2 rounded" %>
      <%= link_to "Cancel", post_path(@post), class: "text-gray-500 ml-2" %>
    </div>
  <% end %>
</turbo-frame>
```

### Modal with Turbo Frame

```erb
<%# app/views/layouts/application.html.erb %>
<%= turbo_frame_tag "modal" %>

<%# Link to open modal %>
<%= link_to "New Post", 
            new_post_path, 
            data: { turbo_frame: "modal" },
            class: "bg-blue-500 text-white px-4 py-2 rounded" %>

<%# app/views/posts/new.html.erb %>
<turbo-frame id="modal">
  <div class="fixed inset-0 bg-black bg-opacity-50 flex items-center justify-center">
    <div class="bg-white p-6 rounded-lg shadow-xl max-w-2xl w-full">
      <h2 class="text-2xl font-bold mb-4">New Post</h2>
      
      <%= form_with model: @post do |f| %>
        <%# Form fields %>
        <%= f.submit %>
      <% end %>
      
      <%= link_to "Close", posts_path, class: "text-gray-500" %>
    </div>
  </div>
</turbo-frame>
```

### Infinite Scroll

```erb
<%# app/views/posts/index.html.erb %>
<div id="posts">
  <%= render @posts %>
</div>

<%= turbo_frame_tag "pagination", 
                    src: posts_path(page: @next_page), 
                    loading: "lazy" if @next_page %>

<%# When this frame loads, it should return more posts + new pagination frame %>
```

---

## Best Practices

1. **Always match frame IDs** between pages
2. **Use `dom_id` helper** for consistent IDs: `dom_id(post)` → `"post_123"`
3. **Handle errors gracefully** - return Turbo Stream responses for errors too
4. **Keep frames focused** - one logical section per frame
5. **Lazy load heavy content** - use `loading="lazy"` for below-fold content
6. **Use broadcasts for real-time** - prefer model callbacks for WebSocket updates
7. **Test without JavaScript** - ensure basic functionality works, then enhance

---

## Troubleshooting

### Frame not updating?
- Check frame IDs match exactly
- Ensure both source and target have `<turbo-frame>` tags
- Check server is returning HTML (not JSON)

### Form submits full page?
- Make sure Turbo is not disabled
- Check form is inside a Turbo Frame if you want frame-scoped updates

### Broadcasts not working?
- Ensure Action Cable is configured
- Check `turbo_stream_from` is in the view
- Verify model callbacks are firing

---

## Reference

- [Turbo Handbook](https://turbo.hotwired.dev/handbook/introduction)
- [Turbo Streams Reference](https://turbo.hotwired.dev/reference/streams)
- Rails `dom_id` helper for consistent IDs
- Rails `turbo_stream` helper for responses
