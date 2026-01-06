---
name: rails-backend-guidelines
description: Rails backend development guidelines for building maintainable Ruby on Rails applications. Use when creating controllers, models, services, concerns, routes, or working with ActiveRecord, background jobs, Action Cable, validations, and Rails conventions. Covers MVC architecture, service objects, RESTful routing, database patterns, and Rails best practices.
---

# Rails Backend Guidelines

## Project Opinions

These are the specific patterns for this project. Follow these over generic Rails advice.

### Fat Models, No Service Objects

**Use models and concerns for business logic. No service objects.**

```ruby
# GOOD: Logic in model
class Post < ApplicationRecord
  def publish!
    transaction do
      update!(published: true, published_at: Time.current)
      notify_subscribers
    end
  end

  private

  def notify_subscribers
    subscribers.find_each { |s| PostMailer.published(self, s).deliver_later }
  end
end

# BAD: Service object
class PostPublisher
  def call(post)
    # Don't do this - put it in the model
  end
end
```

### Concerns for Shared Behavior

Extract shared model behavior to concerns, not service objects:

```ruby
# app/models/concerns/publishable.rb
module Publishable
  extend ActiveSupport::Concern

  included do
    scope :published, -> { where(published: true) }
    scope :draft, -> { where(published: false) }
  end

  def publish!
    update!(published: true, published_at: Time.current)
  end
end
```

### POROs When Needed

When you need a plain object (not ActiveRecord), use a simple PORO:

```ruby
# app/models/webhook_payload.rb
class WebhookPayload
  attr_reader :event, :data

  def initialize(raw_json)
    parsed = JSON.parse(raw_json)
    @event = parsed["event"]
    @data = parsed["data"]
  end

  def valid?
    event.present? && data.present?
  end
end
```

### Controller Pattern

Controllers stay thin. They handle HTTP concerns only:

```ruby
class PostsController < ApplicationController
  def create
    @post = current_user.posts.build(post_params)

    if @post.save
      redirect_to @post, notice: 'Created.'
    else
      render :new, status: :unprocessable_entity
    end
  end

  def publish
    @post = current_user.posts.find(params[:id])
    @post.publish!  # Model handles the logic
    redirect_to @post, notice: 'Published.'
  end
end
```

---

## Resources

For specific patterns when needed:

- [Routing & Controllers](resources/routing-and-controllers.md)
- [Database Patterns](resources/database-patterns.md)
- [Testing Guide](resources/testing-guide.md)
- [Webhook Implementation](resources/webhook-implementation.md)
- [Async & Errors](resources/async-and-errors.md)
