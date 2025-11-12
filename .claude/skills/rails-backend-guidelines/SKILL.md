---
name: rails-backend-guidelines
description: Rails backend development guidelines for building maintainable Ruby on Rails applications. Use when creating controllers, models, services, concerns, routes, or working with ActiveRecord, background jobs, Action Cable, validations, and Rails conventions. Covers MVC architecture, service objects, fat model vs skinny controller debate, RESTful routing, database patterns, and Rails best practices.
---

# Rails Backend Development Guidelines

## Purpose

Establish consistency and best practices for Rails backend development using modern Rails patterns, conventions, and community standards.

## When to Use This Skill

Automatically activates when working on:

- Creating or modifying controllers and actions
- Building models and ActiveRecord queries
- Implementing service objects
- Database migrations and schema
- Background jobs with Solid Queue
- Action Cable (WebSocket) integration
- Rails concerns and modules
- RESTful routing and API endpoints
- Input validation and strong parameters

---

## Quick Start

### New Feature Checklist

- [ ] **Route**: RESTful route definition
- [ ] **Controller**: Skinny controller with strong params
- [ ] **Model**: Validations, associations, scopes
- [ ] **Service**: Complex business logic (if needed)
- [ ] **Migration**: Database schema changes
- [ ] **Tests**: Model, controller, and integration tests
- [ ] **Authorization**: Ensure proper permissions

### New Model Checklist

- [ ] Validations for data integrity
- [ ] Associations (has_many, belongs_to, etc.)
- [ ] Scopes for common queries
- [ ] Callbacks (if absolutely necessary)
- [ ] Custom methods for business logic
- [ ] Database indexes for performance
- [ ] Tests for all validations and methods

---

## Architecture Overview

### MVC Pattern

```
HTTP Request
    ↓
Routes (config/routes.rb)
    ↓
Controller (handles request/response)
    ↓
Model/Service (business logic + data)
    ↓
Database (ActiveRecord → PostgreSQL)
```

**Key Principles:**

- **Skinny Controllers**: Minimal logic, delegate to models/services
- **Fat Models** OR **Service Objects**: Choose based on complexity
- **RESTful Routes**: Follow Rails conventions
- **Strong Parameters**: Always use for security

See [architecture-overview.md](resources/architecture-overview.md) for complete details.

---

## Directory Structure

```
app/
├── controllers/          # Request handlers
│   ├── application_controller.rb
│   ├── posts_controller.rb
│   └── concerns/         # Shared controller code
├── models/               # Data & business logic
│   ├── application_record.rb
│   ├── post.rb
│   └── concerns/         # Shared model code
├── services/             # Complex business logic
│   └── post_publisher.rb
├── jobs/                 # Background jobs
│   └── post_notification_job.rb
├── channels/             # Action Cable channels
│   └── posts_channel.rb
├── mailers/              # Email senders
│   └── user_mailer.rb
├── views/                # Templates
└── helpers/              # View helpers

config/
├── routes.rb             # Route definitions
└── database.yml          # Database config

db/
├── migrate/              # Database migrations
└── schema.rb             # Current schema
```

---

## Core Principles

### 1. Skinny Controllers, Smart Models

```ruby
# ❌ NEVER: Fat controller
class PostsController < ApplicationController
  def create
    @post = Post.new(post_params)
    @post.author = current_user
    @post.published_at = Time.current if params[:publish]
    @post.slug = @post.title.parameterize

    if @post.save
      # 50 more lines...
    end
  end
end

# ✅ ALWAYS: Skinny controller
class PostsController < ApplicationController
  def create
    @post = Post.create_with_author(post_params, current_user)

    if @post.persisted?
      redirect_to @post, notice: "Post created!"
    else
      render :new, status: :unprocessable_entity
    end
  end
end

# Model handles logic
class Post < ApplicationRecord
  def self.create_with_author(params, user)
    post = new(params)
    post.author = user
    post.set_defaults
    post.save
    post
  end
end
```

### 2. Use Service Objects for Complex Logic

```ruby
# When a controller action or model method becomes too complex
class PostPublisher
  def initialize(post, user)
    @post = post
    @user = user
  end

  def publish
    return false unless @post.draft?

    ActiveRecord::Base.transaction do
      @post.update!(published_at: Time.current, status: :published)
      notify_subscribers
      update_search_index
      log_publication
    end

    true
  rescue => e
    Rails.logger.error "Publication failed: #{e.message}"
    false
  end

  private

  def notify_subscribers
    PostNotificationJob.perform_later(@post.id)
  end

  def update_search_index
    # Search indexing logic
  end

  def log_publication
    # Logging logic
  end
end

# Usage in controller
def publish
  if PostPublisher.new(@post, current_user).publish
    redirect_to @post, notice: "Published!"
  else
    redirect_to @post, alert: "Publication failed"
  end
end
```

### 3. Always Use Strong Parameters

```ruby
class PostsController < ApplicationController
  private

  def post_params
    params.require(:post).permit(:title, :content, :status, tag_ids: [])
  end
end
```

### 4. Use Scopes for Common Queries

```ruby
class Post < ApplicationRecord
  scope :published, -> { where.not(published_at: nil) }
  scope :recent, -> { order(created_at: :desc) }
  scope :by_author, ->(user) { where(author: user) }

  # Chainable
  # Post.published.recent.by_author(current_user)
end
```

### 5. Validate All Input

```ruby
class Post < ApplicationRecord
  validates :title, presence: true, length: { maximum: 200 }
  validates :content, presence: true
  validates :slug, uniqueness: true, format: { with: /\A[a-z0-9-]+\z/ }

  validates :status, inclusion: { in: %w[draft published archived] }

  # Custom validation
  validate :published_at_cannot_be_in_past

  private

  def published_at_cannot_be_in_past
    if published_at.present? && published_at < Time.current
      errors.add(:published_at, "can't be in the past")
    end
  end
end
```

### 6. Use Concerns for Shared Behavior

```ruby
# app/models/concerns/publishable.rb
module Publishable
  extend ActiveSupport::Concern

  included do
    scope :published, -> { where.not(published_at: nil) }
    scope :draft, -> { where(published_at: nil) }
  end

  def publish!
    update!(published_at: Time.current)
  end

  def published?
    published_at.present?
  end
end

# Usage
class Post < ApplicationRecord
  include Publishable
end

class Article < ApplicationRecord
  include Publishable
end
```

### 7. Background Jobs for Slow Operations

```ruby
# app/jobs/post_notification_job.rb
class PostNotificationJob < ApplicationJob
  queue_as :default

  def perform(post_id)
    post = Post.find(post_id)
    post.subscribers.each do |subscriber|
      UserMailer.new_post_notification(subscriber, post).deliver_later
    end
  end
end

# Enqueue from controller/model
PostNotificationJob.perform_later(@post.id)
```

---

## Common Imports

```ruby
# Controllers
class ApplicationController < ActionController::Base
end

# Models
class ApplicationRecord < ActiveRecord::Base
  primary_abstract_class
end

# Services
# (Plain Ruby classes, no special inheritance)

# Jobs
class ApplicationJob < ActiveJob::Base
end

# Mailers
class ApplicationMailer < ActionMailer::Base
end

# Channels
class ApplicationCable::Channel < ActionCable::Channel::Base
end
```

---

## Quick Reference

### HTTP Status Codes (Symbols)

| Symbol                   | Code | Use Case          |
| ------------------------ | ---- | ----------------- |
| `:ok`                    | 200  | Success           |
| `:created`               | 201  | Resource created  |
| `:no_content`            | 204  | Success, no body  |
| `:bad_request`           | 400  | Invalid params    |
| `:unauthorized`          | 401  | Not authenticated |
| `:forbidden`             | 403  | Not authorized    |
| `:not_found`             | 404  | Resource missing  |
| `:unprocessable_entity`  | 422  | Validation failed |
| `:internal_server_error` | 500  | Server error      |

### Common Rails Patterns

**CRUD Controller Actions**: `index`, `show`, `new`, `create`, `edit`, `update`, `destroy`

**RESTful Routes**: `resources :posts` generates 7 routes automatically

**Callbacks**: `before_validation`, `after_save`, `before_destroy` (use sparingly!)

---

## Anti-Patterns to Avoid

❌ Fat controllers with business logic
❌ Direct SQL strings (use ActiveRecord)
❌ Callbacks for everything (hard to test)
❌ N+1 queries (use `includes`, `eager_load`)
❌ Mass assignment without strong params
❌ God objects (models that do everything)
❌ Ignoring Rails conventions
❌ console.log debugging in production

---

## Navigation Guide

| Need to...              | Read this                                                      |
| ----------------------- | -------------------------------------------------------------- |
| Understand Rails MVC    | [architecture-overview.md](resources/architecture-overview.md) |
| Create controllers      | [controllers-guide.md](resources/controllers-guide.md)         |
| Design models           | [models-guide.md](resources/models-guide.md)                   |
| Extract service objects | [service-objects.md](resources/service-objects.md)             |
| Database & migrations   | [database-guide.md](resources/database-guide.md)               |
| Background jobs         | [jobs-guide.md](resources/jobs-guide.md)                       |
| RESTful routing         | [routing-guide.md](resources/routing-guide.md)                 |
| Testing strategies      | [testing-guide.md](resources/testing-guide.md)                 |
| Action Cable            | [action-cable-guide.md](resources/action-cable-guide.md)       |
| See complete examples   | [complete-examples.md](resources/complete-examples.md)         |

---

## Resource Files

### [architecture-overview.md](resources/architecture-overview.md)

MVC pattern, request lifecycle, Rails conventions

### [controllers-guide.md](resources/controllers-guide.md)

Skinny controllers, strong params, respond_to, filters

### [models-guide.md](resources/models-guide.md)

ActiveRecord, validations, associations, scopes, callbacks

### [service-objects.md](resources/service-objects.md)

When to use services, patterns, examples

### [database-guide.md](resources/database-guide.md)

Migrations, indexes, queries, N+1 prevention

### [jobs-guide.md](resources/jobs-guide.md)

Solid Queue, background processing, job patterns

### [routing-guide.md](resources/routing-guide.md)

RESTful routes, nested resources, custom routes

### [testing-guide.md](resources/testing-guide.md)

Model tests, controller tests, system tests, fixtures

### [action-cable-guide.md](resources/action-cable-guide.md)

WebSocket channels, real-time features, broadcasting

### [complete-examples.md](resources/complete-examples.md)

Full CRUD examples, real-world patterns

---

## Related Skills

- **rails-frontend-guidelines** - Hotwire, Turbo, Stimulus patterns
- **skill-developer** - Meta-skill for creating skills

---

**Skill Status**: Adapted for Rails 7+ with modern patterns
