# Routing and Controllers (37signals Style)

Thin controllers, rich models, composable concerns. Controllers orchestrate; models execute.

---

## Routing: Everything is CRUD

Map all actions to Create, Read, Update, or Destroy. When something doesn't fit, create a new resource instead of adding custom actions.

### Convert Verbs to Nouns

Don't add custom actions—create resources:

```ruby
# BAD: Custom actions
resources :cards do
  post :close
  post :reopen
  post :archive
end

# GOOD: Noun-based resources
resources :cards do
  resource :closure, only: [:create, :destroy]  # POST closes, DELETE reopens
  resource :archival, only: [:create, :destroy]
end

# More examples:
# "Watch a board" → board.watching
# "Pin an item" → item.pin
# "Publish a post" → post.publication
```

### Singular vs Plural Resources

Use `resource` (singular) for one-per-parent relationships:

```ruby
resources :cards do
  resource :closure, only: [:create, :destroy]     # One closure per card
  resources :comments                               # Many comments per card
end
```

### Shallow Nesting

Avoid deeply nested URLs with `shallow: true`:

```ruby
resources :boards, shallow: true do
  resources :cards do
    resources :comments
  end
end

# Produces:
# /boards/:board_id/cards (index, create)
# /cards/:id (show, update, destroy) - shallow!
# /cards/:card_id/comments (index, create)
# /comments/:id (show, update, destroy) - shallow!
```

### Module Scoping

Group related controllers without changing URLs:

```ruby
# Same URLs, organized controllers
scope module: :admin do
  resources :users  # Admin::UsersController, /users
end

# URL prefix + organized controllers
namespace :admin do
  resources :users  # Admin::UsersController, /admin/users
end
```

### API Responses

Same controllers handle both HTML and JSON—no separate API namespace:

```ruby
def create
  @card = @board.cards.create!(card_params)

  respond_to do |format|
    format.html { redirect_to @card }
    format.json { render json: @card, status: :created, location: @card }
  end
end

def destroy
  @card.destroy!

  respond_to do |format|
    format.html { redirect_to board_cards_path(@board) }
    format.json { head :no_content }
  end
end
```

---

## Controllers: Thin and Composable

### The Pattern

Controllers handle HTTP concerns only. Business logic lives in models.

```ruby
class CardsController < ApplicationController
  include BoardScoped  # Loads @board, handles auth

  def create
    @card = @board.cards.create!(card_params)
    redirect_to @card
  end

  def update
    @card = @board.cards.find(params[:id])
    @card.update!(card_params)
    render_card_replacement  # From BoardScoped concern
  end
end
```

### Authorization Pattern

Controllers **check**; models **define**:

```ruby
# Controller checks
class CardsController < ApplicationController
  before_action :ensure_can_administer, only: [:edit, :update, :destroy]

  private

  def ensure_can_administer
    unless Current.user.can_administer_card?(@card)
      redirect_to @card, alert: "Not authorized"
    end
  end
end

# Model defines what "administer" means
class User < ApplicationRecord
  def can_administer_card?(card)
    card.board.administrators.include?(self) || card.creator == self
  end
end
```

---

## Controller Concerns

### Resource Scoping

Load parent resources and provide shared methods:

```ruby
# app/controllers/concerns/board_scoped.rb
module BoardScoped
  extend ActiveSupport::Concern

  included do
    before_action :set_board
    before_action :ensure_board_access
  end

  private

  def set_board
    @board = Current.account.boards.find(params[:board_id])
  end

  def ensure_board_access
    unless Current.user.can_access_board?(@board)
      redirect_to boards_path, alert: "Access denied"
    end
  end

  # Shared rendering for Turbo responses
  def render_card_replacement(card = @card)
    render turbo_stream: turbo_stream.replace(card)
  end
end
```

```ruby
# app/controllers/concerns/card_scoped.rb
module CardScoped
  extend ActiveSupport::Concern
  include BoardScoped

  included do
    before_action :set_card
  end

  private

  def set_card
    @card = @board.cards.find(params[:card_id])
  end
end
```

### Request Context

Populate `Current` with request metadata:

```ruby
# app/controllers/concerns/current_request.rb
module CurrentRequest
  extend ActiveSupport::Concern

  included do
    before_action :set_current_request_details
  end

  private

  def set_current_request_details
    Current.request_id = request.request_id
    Current.user_agent = request.user_agent
    Current.ip_address = request.remote_ip
  end
end
```

### Timezone Handling

Wrap requests in user's timezone:

```ruby
# app/controllers/concerns/current_timezone.rb
module CurrentTimezone
  extend ActiveSupport::Concern

  included do
    around_action :set_timezone
  end

  private

  def set_timezone(&block)
    timezone = Current.user&.timezone || cookies[:timezone] || "UTC"
    Time.use_zone(timezone, &block)
  end
end
```

---

## ApplicationController

Compose concerns for shared behavior:

```ruby
class ApplicationController < ActionController::Base
  include CurrentRequest
  include CurrentTimezone
  include Authentication  # Your auth concern

  # Security
  protect_from_forgery with: :exception

  # Turbo-friendly flash
  add_flash_types :success, :warning, :error
end
```

---

## Composable Rendering

Scoping concerns provide reusable render helpers:

```ruby
module BoardScoped
  # ... scoping logic ...

  def render_board_update
    render turbo_stream: turbo_stream.replace(@board)
  end

  def render_card_append(card)
    render turbo_stream: turbo_stream.append("cards", card)
  end

  def render_card_removal(card)
    render turbo_stream: turbo_stream.remove(card)
  end
end

# Multiple controllers use the same rendering
class CardsController < ApplicationController
  include BoardScoped

  def create
    @card = @board.cards.create!(card_params)
    render_card_append(@card)
  end

  def destroy
    @card.destroy
    render_card_removal(@card)
  end
end

class Card::ArchivalsController < ApplicationController
  include CardScoped

  def create
    @card.archive!
    render_card_removal(@card)  # Same helper, different context
  end
end
```

---

## Quick Reference

| Pattern | Example |
|---------|---------|
| Custom action → Resource | `post :close` → `resource :closure` |
| Verb → Noun | "watch" → `watching`, "pin" → `pin` |
| One-per-parent | `resource :closure` (singular) |
| Many-per-parent | `resources :comments` (plural) |
| Auth check | Controller calls `Current.user.can_X?` |
| Auth logic | Model defines `can_X?` method |
| Shared loading | `include BoardScoped` |
| Shared rendering | `render_card_replacement` from concern |
