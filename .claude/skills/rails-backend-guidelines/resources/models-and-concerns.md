# Models and Concerns (37signals Style)

Rich domain models using composable concerns. Business logic lives in models, not service objects.

---

## Concerns for Horizontal Behavior

Models include many focused concerns, each handling one aspect:

```ruby
class Card < ApplicationRecord
  include Assignable
  include Attachable
  include Broadcastable
  include Closeable
  include Commentable
  include Copyable
  include Goldable
  include Movable
  include Notifiable
  include Searchable
  include Sortable
  include Subscribable
  # Main model stays minimal
end
```

### Self-Contained Concerns

Each concern owns its associations, scopes, and methods:

```ruby
# app/models/concerns/closeable.rb
module Closeable
  extend ActiveSupport::Concern

  included do
    has_one :closure, dependent: :destroy

    scope :closed, -> { joins(:closure) }
    scope :open, -> { where.missing(:closure) }
  end

  def closed?
    closure.present?
  end

  def open?
    !closed?
  end

  def close(by: Current.user)
    create_closure!(closer: by)
  end

  def reopen
    closure&.destroy!
  end
end
```

---

## State as Records (Not Booleans)

Instead of boolean columns, create separate record models:

```ruby
# BAD: Boolean flag
class Card < ApplicationRecord
  # closed: boolean
  # closed_at: datetime
  # closed_by_id: integer
end

# GOOD: State as record
class Closure < ApplicationRecord
  belongs_to :card
  belongs_to :closer, class_name: "User"
  # Automatically has created_at for when it was closed
end
```

### More Examples

```ruby
# Card importance
class Card::Goldness < ApplicationRecord
  belongs_to :card
  belongs_to :marker, class_name: "User"
end

# Postponed cards
class Card::NotNow < ApplicationRecord
  belongs_to :card
  belongs_to :postponer, class_name: "User"
end

# Public boards with secure tokens
class Board::Publication < ApplicationRecord
  belongs_to :board
  has_secure_token :share_token
end
```

### Why State as Records?

1. **Rich querying** via `joins()` and `where.missing()`
2. **Metadata capture** — who did it, when, why
3. **Audit trail** built-in
4. **Cleaner scopes** — `scope :closed, -> { joins(:closure) }`

---

## Default Values via Lambdas

Use lambda defaults to pull from context:

```ruby
class Card < ApplicationRecord
  belongs_to :account, default: -> { board.account }
  belongs_to :creator, default: -> { Current.user }
  belongs_to :column, default: -> { board.columns.first }
end

class Comment < ApplicationRecord
  belongs_to :card
  belongs_to :author, default: -> { Current.user }
end
```

---

## Current Attributes for Request Context

Use `ActiveSupport::CurrentAttributes` instead of passing params everywhere:

```ruby
# app/models/current.rb
class Current < ActiveSupport::CurrentAttributes
  attribute :session
  attribute :user
  attribute :account
  attribute :request_id
  attribute :user_agent
  attribute :ip_address

  delegate :identity, to: :session, allow_nil: true

  def user
    session&.user
  end
end
```

```ruby
# Set in ApplicationController
class ApplicationController < ActionController::Base
  before_action :set_current_attributes

  private

  def set_current_attributes
    Current.session = current_session
    Current.account = current_account
    Current.request_id = request.request_id
  end
end
```

```ruby
# Use anywhere in models
class Card < ApplicationRecord
  belongs_to :creator, default: -> { Current.user }

  def close
    create_closure!(closer: Current.user)
  end
end
```

---

## Minimal Validations

Keep validations simple. Use contextual validations for multi-step processes:

```ruby
class Card < ApplicationRecord
  validates :name, presence: true

  # Context-specific validation
  validates :description, presence: true, on: :publish
end

# Usage
card.save                    # Only validates name
card.save(context: :publish) # Validates name and description
```

---

## Bang Methods (Let It Crash)

Use `create!` and `update!` to raise exceptions on failure:

```ruby
# BAD: Silent failure
def close
  create_closure(closer: Current.user)
  # Returns false on failure, easy to miss
end

# GOOD: Loud failure
def close
  create_closure!(closer: Current.user)
  # Raises ActiveRecord::RecordInvalid on failure
end
```

---

## Sparse Callbacks

Callbacks for setup/cleanup only—never business logic:

```ruby
class Card < ApplicationRecord
  # GOOD: Setup
  after_create_commit :broadcast_created
  before_destroy :cleanup_attachments

  # BAD: Business logic in callback
  # after_save :send_notifications_if_closed  # Don't do this
  # after_save :update_board_stats            # Don't do this
end
```

When you need business logic triggered by state changes, make it explicit:

```ruby
# Explicit in the method
def close
  create_closure!(closer: Current.user)
  notify_watchers           # Explicit, visible
  broadcast_replacement     # Explicit, visible
end
```

---

## POROs in Model Namespaces

Plain Old Ruby Objects for non-persistent logic, namespaced under models:

### Presentation Objects

```ruby
# app/models/event/description.rb
class Event::Description
  def initialize(event)
    @event = event
  end

  def to_s
    case @event.action
    when "created" then "#{actor_name} created #{target_name}"
    when "closed" then "#{actor_name} closed #{target_name}"
    else "#{actor_name} #{@event.action} #{target_name}"
    end
  end

  private

  def actor_name
    @event.actor&.name || "Someone"
  end

  def target_name
    @event.target&.name || "something"
  end
end
```

### Complex Operations

```ruby
# app/models/system_commenter.rb
class SystemCommenter
  def initialize(card:, action:)
    @card = card
    @action = action
  end

  def post
    @card.comments.create!(
      author: nil,
      body: message,
      system: true
    )
  end

  private

  def message
    case @action
    when :closed then "Card was closed by #{Current.user.name}"
    when :moved then "Card was moved to #{@card.column.name}"
    end
  end
end

# Usage
SystemCommenter.new(card: @card, action: :closed).post
```

### View Context Objects

```ruby
# app/models/card/details.rb
class Card::Details
  def initialize(card)
    @card = card
  end

  def assignees
    @card.assignees.includes(:avatar_attachment)
  end

  def recent_comments
    @card.comments.recent.limit(5)
  end

  def attachment_count
    @card.attachments.count
  end
end
```

---

## Semantic Scope Names

Use business-focused names, not implementation details:

```ruby
class Card < ApplicationRecord
  # GOOD: Business meaning
  scope :open, -> { where.missing(:closure) }
  scope :closed, -> { joins(:closure) }
  scope :active, -> { open.where.missing(:not_now) }
  scope :postponed, -> { joins(:not_now) }
  scope :recently_closed_first, -> { closed.order("closures.created_at DESC") }
  scope :gold, -> { joins(:goldness) }

  # BAD: Implementation details
  # scope :without_closure, -> { ... }
  # scope :has_goldness_record, -> { ... }
end
```

---

## Quick Reference

| Pattern | Example |
|---------|---------|
| State as record | `Closure`, `Publication`, `Goldness` |
| Concern scope | `scope :closed, -> { joins(:closure) }` |
| Lambda default | `belongs_to :creator, default: -> { Current.user }` |
| Current attributes | `Current.user`, `Current.account` |
| Bang methods | `create!`, `update!`, `destroy!` |
| PORO namespace | `Card::Details`, `Event::Description` |
| Semantic scopes | `:open`, `:closed`, `:active`, `:gold` |
