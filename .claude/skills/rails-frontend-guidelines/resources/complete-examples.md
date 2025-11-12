# Complete Examples

Full working examples demonstrating Rails frontend patterns.

## Example 1: CRUD with Turbo Frames

### Index View with Inline Editing

```erb
<%# app/views/posts/index.html.erb %>
<div class="max-w-6xl mx-auto p-6">
  <div class="flex justify-between items-center mb-6">
    <h1 class="text-3xl font-bold">Posts</h1>

    <%= link_to "New Post",
                new_post_path,
                data: { turbo_frame: "modal" },
                class: "bg-blue-500 hover:bg-blue-700 text-white font-bold py-2 px-4 rounded" %>
  </div>

  <div id="posts" class="space-y-4">
    <%= render @posts %>
  </div>
</div>

<%# Modal placeholder %>
<%= turbo_frame_tag "modal" %>
```

### Post Partial

```erb
<%# app/views/posts/_post.html.erb %>
<%= turbo_frame_tag dom_id(post) do %>
  <div class="bg-white rounded-lg shadow-md p-6 hover:shadow-lg transition-shadow">
    <div class="flex justify-between items-start">
      <div class="flex-1">
        <h2 class="text-2xl font-bold mb-2"><%= post.title %></h2>
        <p class="text-gray-700 mb-4"><%= truncate(post.content, length: 200) %></p>
        <p class="text-sm text-gray-500">
          Posted <%= time_ago_in_words(post.created_at) %> ago
        </p>
      </div>

      <div class="flex gap-2 ml-4">
        <%= link_to "Edit",
                    edit_post_path(post),
                    class: "text-blue-500 hover:text-blue-700" %>

        <%= button_to "Delete",
                      post_path(post),
                      method: :delete,
                      form: { data: { turbo_confirm: "Are you sure?" } },
                      class: "text-red-500 hover:text-red-700" %>
      </div>
    </div>
  </div>
<% end %>
```

### Edit View (Inline)

```erb
<%# app/views/posts/edit.html.erb %>
<%= turbo_frame_tag dom_id(@post) do %>
  <div class="bg-white rounded-lg shadow-md p-6">
    <h2 class="text-2xl font-bold mb-4">Edit Post</h2>

    <%= form_with model: @post, class: "space-y-4" do |f| %>
      <div>
        <%= f.label :title, class: "block font-bold mb-2" %>
        <%= f.text_field :title,
                         class: "border rounded-lg p-2 w-full focus:ring-2 focus:ring-blue-500" %>
      </div>

      <div>
        <%= f.label :content, class: "block font-bold mb-2" %>
        <%= f.text_area :content,
                        rows: 6,
                        class: "border rounded-lg p-2 w-full focus:ring-2 focus:ring-blue-500" %>
      </div>

      <div class="flex gap-2">
        <%= f.submit "Save",
                     class: "bg-blue-500 hover:bg-blue-700 text-white font-bold py-2 px-4 rounded" %>

        <%= link_to "Cancel",
                    post_path(@post),
                    class: "bg-gray-300 hover:bg-gray-400 text-gray-800 font-bold py-2 px-4 rounded" %>
      </div>
    <% end %>
  </div>
<% end %>
```

### New Post Modal

```erb
<%# app/views/posts/new.html.erb %>
<%= turbo_frame_tag "modal" do %>
  <div class="fixed inset-0 bg-black bg-opacity-50 flex items-center justify-center z-50"
       data-controller="modal"
       data-action="click->modal#closeBackground">

    <div class="bg-white rounded-lg shadow-xl max-w-2xl w-full m-4"
         data-modal-target="content">
      <div class="p-6">
        <div class="flex justify-between items-center mb-4">
          <h2 class="text-2xl font-bold">New Post</h2>

          <%= link_to posts_path,
                      class: "text-gray-500 hover:text-gray-700" do %>
            <svg class="w-6 h-6" fill="none" stroke="currentColor" viewBox="0 0 24 24">
              <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M6 18L18 6M6 6l12 12" />
            </svg>
          <% end %>
        </div>

        <%= form_with model: @post, class: "space-y-4" do |f| %>
          <div>
            <%= f.label :title, class: "block font-bold mb-2" %>
            <%= f.text_field :title,
                             class: "border rounded-lg p-2 w-full focus:ring-2 focus:ring-blue-500" %>
          </div>

          <div>
            <%= f.label :content, class: "block font-bold mb-2" %>
            <%= f.text_area :content,
                            rows: 8,
                            class: "border rounded-lg p-2 w-full focus:ring-2 focus:ring-blue-500" %>
          </div>

          <div class="flex gap-2">
            <%= f.submit "Create Post",
                         class: "bg-blue-500 hover:bg-blue-700 text-white font-bold py-2 px-4 rounded" %>

            <%= link_to "Cancel",
                        posts_path,
                        class: "bg-gray-300 hover:bg-gray-400 text-gray-800 font-bold py-2 px-4 rounded" %>
          </div>
        <% end %>
      </div>
    </div>
  </div>
<% end %>
```

### Modal Stimulus Controller

```javascript
// app/javascript/controllers/modal_controller.js
import { Controller } from "@hotwired/stimulus";

export default class extends Controller {
  static targets = ["content"];

  closeBackground(event) {
    if (event.target === this.element) {
      this.close();
    }
  }

  close() {
    this.element.remove();
  }

  // Close on Escape key
  connect() {
    this.boundHandleEscape = this.handleEscape.bind(this);
    document.addEventListener("keydown", this.boundHandleEscape);
  }

  disconnect() {
    document.removeEventListener("keydown", this.boundHandleEscape);
  }

  handleEscape(event) {
    if (event.key === "Escape") {
      this.close();
    }
  }
}
```

### Controller with Turbo Streams

```ruby
# app/controllers/posts_controller.rb
class PostsController < ApplicationController
  def index
    @posts = Post.order(created_at: :desc)
  end

  def new
    @post = Post.new
  end

  def create
    @post = Post.new(post_params)

    respond_to do |format|
      if @post.save
        format.turbo_stream do
          render turbo_stream: [
            turbo_stream.prepend("posts", partial: "posts/post", locals: { post: @post }),
            turbo_stream.remove("modal")
          ]
        end
        format.html { redirect_to posts_path, notice: "Post created!" }
      else
        format.html { render :new, status: :unprocessable_entity }
      end
    end
  end

  def edit
    @post = Post.find(params[:id])
  end

  def update
    @post = Post.find(params[:id])

    respond_to do |format|
      if @post.update(post_params)
        format.turbo_stream do
          render turbo_stream: turbo_stream.replace(
            dom_id(@post),
            partial: "posts/post",
            locals: { post: @post }
          )
        end
        format.html { redirect_to @post, notice: "Post updated!" }
      else
        format.html { render :edit, status: :unprocessable_entity }
      end
    end
  end

  def destroy
    @post = Post.find(params[:id])
    @post.destroy

    respond_to do |format|
      format.turbo_stream { render turbo_stream: turbo_stream.remove(dom_id(@post)) }
      format.html { redirect_to posts_path, notice: "Post deleted!" }
    end
  end

  private

  def post_params
    params.require(:post).permit(:title, :content)
  end
end
```

---

## Example 2: Real-time Comments with Action Cable

### Post Show with Comments

```erb
<%# app/views/posts/show.html.erb %>
<div class="max-w-4xl mx-auto p-6">
  <article class="bg-white rounded-lg shadow-md p-8 mb-6">
    <h1 class="text-4xl font-bold mb-4"><%= @post.title %></h1>
    <div class="prose max-w-none">
      <%= simple_format @post.content %>
    </div>
  </article>

  <section class="bg-white rounded-lg shadow-md p-6">
    <h2 class="text-2xl font-bold mb-4">Comments</h2>

    <%# Subscribe to real-time updates %>
    <%= turbo_stream_from @post %>

    <%# New comment form %>
    <%= turbo_frame_tag "new_comment" do %>
      <%= render "comments/form", post: @post, comment: Comment.new %>
    <% end %>

    <%# Comments list %>
    <div id="comments" class="space-y-4 mt-6">
      <%= render @post.comments.order(created_at: :desc) %>
    </div>
  </section>
</div>
```

### Comment Partial

```erb
<%# app/views/comments/_comment.html.erb %>
<%= turbo_frame_tag dom_id(comment) do %>
  <div class="bg-gray-50 rounded-lg p-4">
    <div class="flex justify-between items-start mb-2">
      <strong class="text-gray-900"><%= comment.author_name %></strong>
      <span class="text-sm text-gray-500">
        <%= time_ago_in_words(comment.created_at) %> ago
      </span>
    </div>

    <p class="text-gray-700"><%= comment.content %></p>

    <div class="mt-2 flex gap-4">
      <%= link_to "Edit",
                  edit_post_comment_path(comment.post, comment),
                  class: "text-blue-500 hover:text-blue-700 text-sm" %>

      <%= button_to "Delete",
                    post_comment_path(comment.post, comment),
                    method: :delete,
                    form: { data: { turbo_confirm: "Are you sure?" } },
                    class: "text-red-500 hover:text-red-700 text-sm" %>
    </div>
  </div>
<% end %>
```

### Comment Form

```erb
<%# app/views/comments/_form.html.erb %>
<%= form_with model: [post, comment],
              data: { controller: "reset-form" },
              class: "space-y-4" do |f| %>

  <div>
    <%= f.label :author_name, "Your Name", class: "block font-bold mb-2" %>
    <%= f.text_field :author_name,
                     class: "border rounded-lg p-2 w-full" %>
  </div>

  <div>
    <%= f.label :content, "Comment", class: "block font-bold mb-2" %>
    <%= f.text_area :content,
                    rows: 3,
                    class: "border rounded-lg p-2 w-full" %>
  </div>

  <%= f.submit "Add Comment",
               class: "bg-blue-500 hover:bg-blue-700 text-white font-bold py-2 px-4 rounded" %>
<% end %>
```

### Comment Model with Broadcasting

```ruby
# app/models/comment.rb
class Comment < ApplicationRecord
  belongs_to :post

  validates :author_name, :content, presence: true

  # Broadcast changes to all connected clients
  after_create_commit { broadcast_prepend_to post, target: "comments" }
  after_update_commit { broadcast_replace_to post }
  after_destroy_commit { broadcast_remove_to post }
end
```

### Comments Controller

```ruby
# app/controllers/comments_controller.rb
class CommentsController < ApplicationController
  before_action :set_post

  def create
    @comment = @post.comments.build(comment_params)

    if @comment.save
      # The after_create_commit callback handles broadcasting
      respond_to do |format|
        format.turbo_stream do
          render turbo_stream: turbo_stream.replace(
            "new_comment",
            partial: "comments/form",
            locals: { post: @post, comment: Comment.new }
          )
        end
        format.html { redirect_to @post, notice: "Comment added!" }
      end
    else
      render :new, status: :unprocessable_entity
    end
  end

  def edit
    @comment = @post.comments.find(params[:id])
  end

  def update
    @comment = @post.comments.find(params[:id])

    if @comment.update(comment_params)
      # The after_update_commit callback handles broadcasting
      respond_to do |format|
        format.turbo_stream
        format.html { redirect_to @post, notice: "Comment updated!" }
      end
    else
      render :edit, status: :unprocessable_entity
    end
  end

  def destroy
    @comment = @post.comments.find(params[:id])
    @comment.destroy

    # The after_destroy_commit callback handles broadcasting
    respond_to do |format|
      format.turbo_stream
      format.html { redirect_to @post, notice: "Comment deleted!" }
    end
  end

  private

  def set_post
    @post = Post.find(params[:post_id])
  end

  def comment_params
    params.require(:comment).permit(:author_name, :content)
  end
end
```

### Reset Form Controller

```javascript
// app/javascript/controllers/reset_form_controller.js
import { Controller } from "@hotwired/stimulus";

export default class extends Controller {
  connect() {
    // Listen for turbo:submit-end event
    this.element.addEventListener(
      "turbo:submit-end",
      this.handleSubmit.bind(this),
    );
  }

  handleSubmit(event) {
    // Reset form if submission was successful
    if (event.detail.success) {
      this.element.reset();
    }
  }
}
```

---

## Example 3: Search with Auto-complete

### Search Form

```erb
<%# app/views/posts/index.html.erb %>
<div class="max-w-6xl mx-auto p-6">
  <div class="mb-6">
    <%= form_with url: posts_path,
                  method: :get,
                  data: {
                    controller: "search",
                    turbo_frame: "search_results"
                  },
                  class: "relative" do |f| %>

      <div class="relative">
        <%= f.search_field :query,
                           value: params[:query],
                           placeholder: "Search posts...",
                           data: {
                             search_target: "input",
                             action: "input->search#search"
                           },
                           class: "border rounded-lg p-3 w-full pr-10" %>

        <div class="absolute right-3 top-3"
             data-search-target="spinner"
             style="display: none;">
          <svg class="animate-spin h-5 w-5 text-gray-500" fill="none" viewBox="0 0 24 24">
            <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
            <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
          </svg>
        </div>
      </div>
    <% end %>
  </div>

  <%= turbo_frame_tag "search_results" do %>
    <div id="posts" class="space-y-4">
      <%= render @posts %>
    </div>
  <% end %>
</div>
```

### Search Controller

```javascript
// app/javascript/controllers/search_controller.js
import { Controller } from "@hotwired/stimulus";

export default class extends Controller {
  static targets = ["input", "spinner"];
  static values = {
    delay: { type: Number, default: 300 },
  };

  connect() {
    this.timeout = null;
  }

  disconnect() {
    clearTimeout(this.timeout);
  }

  search() {
    clearTimeout(this.timeout);

    // Show spinner
    this.spinnerTarget.style.display = "block";

    // Debounce search
    this.timeout = setTimeout(() => {
      this.element.requestSubmit();
    }, this.delayValue);
  }
}
```

### Controller with Turbo Frame Response

```ruby
# app/controllers/posts_controller.rb
class PostsController < ApplicationController
  def index
    @posts = if params[:query].present?
      Post.where("title ILIKE ? OR content ILIKE ?",
                 "%#{params[:query]}%",
                 "%#{params[:query]}%")
          .order(created_at: :desc)
    else
      Post.order(created_at: :desc)
    end

    # Turbo Frame will automatically extract the matching frame
    render :index
  end
end
```

---

These examples demonstrate real-world patterns you'll use in Rails applications with Hotwire. They show progressive enhancement, real-time updates, and minimal JavaScript for maximum effect.
