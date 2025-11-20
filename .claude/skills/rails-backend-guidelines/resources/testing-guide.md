# Testing Guide - Rails Testing with Minitest

Complete guide to testing Rails applications with Minitest and best practices.

## Table of Contents

- [Running Tests](#running-tests)
- [Test Structure](#test-structure)
- [Controller Testing](#controller-testing)
- [Model Testing](#model-testing)
- [Test Fixtures](#test-fixtures)
- [Webhook Testing](#webhook-testing)
- [Testing Best Practices](#testing-best-practices)

---

## Running Tests

### Basic Commands

```bash
# Run all tests
bin/rails test

# Run a specific test file
bin/rails test test/controllers/github/webhooks_controller_test.rb

# Run a specific test by name
bin/rails test test/controllers/github/webhooks_controller_test.rb -n test_name

# Run tests with verbose output
bin/rails test --verbose
```

**IMPORTANT:**

- Use `bin/rails test path/to/test.rb` NOT `bin/rails test path/to/test.rb::test_name`
- Use `-n test_name` flag to run a specific test
- Always prefix with `bin/rails` to ensure proper environment setup

### Test Database Management

```bash
# Reset test database (drops, creates, loads schema, loads fixtures)
RAILS_ENV=test bin/rails db:reset

# Run migrations on test database
RAILS_ENV=test bin/rails db:migrate

# Load fixtures
RAILS_ENV=test bin/rails db:fixtures:load
```

---

## Test Structure

### Basic Test File Structure

```ruby
# frozen_string_literal: true

require "test_helper"

module Github
  class WebhooksControllerTest < ActionDispatch::IntegrationTest
    # Load only the fixtures you need
    fixtures :"bot/messages"

    setup do
      # Setup code runs before each test
      @webhook_secret = "test_secret"
      ENV["GITHUB_WEBHOOK_SECRET"] = @webhook_secret
    end

    teardown do
      # Cleanup code runs after each test
      ENV.delete("GITHUB_WEBHOOK_SECRET")
    end

    test "descriptive test name" do
      # Test code here
      assert true
    end
  end
end
```

### Test Naming Conventions

- Use descriptive names: `test "issue_comment webhook creates bot message"`
- Group related tests with prefixes: `test "extract_linked_issues finds Fixes references"`
- Use underscores for method names: `test_method_name` or spaces in strings: `test "method name"`

---

## Controller Testing

### Integration Test Example

```ruby
test "issue_comment webhook creates bot message with prompt field" do
  payload = {
    action: "created",
    repository: { full_name: "owner/repo" },
    issue: {
      number: 123,
      title: "Test issue",
      body: "Issue body",
      html_url: "https://github.com/owner/repo/issues/123",
      pull_request: nil
    },
    comment: {
      id: 456,
      body: "@trybotster please help",
      user: { login: "testuser" }
    }
  }

  body, signature = sign_webhook_payload(payload)

  post "/github/webhooks",
    params: body,
    headers: {
      "Content-Type" => "application/json",
      "X-GitHub-Event" => "issue_comment",
      "X-Hub-Signature-256" => signature
    }

  assert_response :success

  message = Bot::Message.last
  assert_equal "github_mention", message.event_type
  assert_not_nil message.payload["prompt"]
end
```

### Testing Private Methods

```ruby
test "extract_linked_issues finds Fixes references" do
  controller = Github::WebhooksController.new

  pr_body = "This PR fixes #123 and resolves #456"
  issues = controller.send(:extract_linked_issues, pr_body)

  assert_equal [123, 456], issues
end
```

### Assertions

```ruby
# Response assertions
assert_response :success
assert_response :not_found
assert_response :unauthorized

# Record count assertions
assert_difference "Bot::Message.count", 1 do
  # Code that creates a record
end

assert_no_difference "Bot::Message.count" do
  # Code that should not create a record
end

# Equality assertions
assert_equal expected, actual
assert_not_equal expected, actual

# Nil assertions
assert_nil value
assert_not_nil value

# Presence assertions
assert value
refute value

# String assertions
assert_includes string, substring
assert_match /regex/, string
```

---

## Model Testing

### ActiveRecord Model Tests

```ruby
class BotMessageTest < ActiveSupport::TestCase
  test "should not save without event_type" do
    message = Bot::Message.new(payload: {})
    assert_not message.save
  end

  test "should save with valid attributes" do
    message = Bot::Message.new(
      event_type: "github_mention",
      payload: { repo: "owner/repo" }
    )
    assert message.save
  end

  test "associations work correctly" do
    message = bot_messages(:one)
    assert_respond_to message, :tags
  end
end
```

---

## Test Fixtures

### Fixture Management

```ruby
# Load all fixtures (default behavior in test_helper.rb)
fixtures :all

# Load specific fixtures only
fixtures :"bot/messages", :users

# Don't load any fixtures by default
# (override the test_helper.rb setting)
self.use_transactional_tests = true
# Don't call fixtures :all
```

### Best Practices for Fixtures

1. **Only load what you need** - Reduces test setup time and avoids dependency issues
2. **Keep fixtures minimal** - Only include required fields
3. **Avoid fixture dependencies** - If possible, don't rely on cross-table fixture relationships
4. **Use unique values** - Avoid duplicate values that violate unique constraints

### Example Fixture File

```yaml
# test/fixtures/bot/messages.yml
one:
  event_type: github_mention
  payload: { repo: "test/repo", issue_number: 1 }
  created_at: <%= 1.day.ago %>
  updated_at: <%= 1.day.ago %>

two:
  event_type: github_mention
  payload: { repo: "test/repo", issue_number: 2 }
  created_at: <%= 2.days.ago %>
  updated_at: <%= 2.days.ago %>
```

### Common Fixture Issues

**Problem:** `ActiveRecord::NotNullViolation`

```ruby
# Solution: Ensure all required fields have values in fixtures
# Or don't load the fixture if it's not needed
```

**Problem:** Duplicate key violations

```ruby
# Solution: Ensure unique constraints are respected in fixtures
# Example: tags.yml had duplicate names, changed to unique values
```

**Problem:** Foreign key violations

```ruby
# Solution: Either include the referenced fixtures or remove the dependency
# Example: memory_tags.yml referenced non-existent memories - deleted it
```

---

## Webhook Testing

### Webhook Signature Verification

```ruby
def sign_webhook_payload(payload)
  body = payload.to_json
  signature = "sha256=" + OpenSSL::HMAC.hexdigest(
    OpenSSL::Digest.new("sha256"),
    @webhook_secret,
    body
  )
  [body, signature]
end

test "webhook with valid signature succeeds" do
  payload = { action: "created", ... }
  body, signature = sign_webhook_payload(payload)

  post "/github/webhooks",
    params: body,
    headers: {
      "Content-Type" => "application/json",
      "X-GitHub-Event" => "issue_comment",
      "X-Hub-Signature-256" => signature
    }

  assert_response :success
end
```

### Testing Environment Variables

```ruby
setup do
  ENV["GITHUB_WEBHOOK_SECRET"] = "test_secret"
end

teardown do
  ENV.delete("GITHUB_WEBHOOK_SECRET")
end
```

### Testing Bot Comment Filtering

```ruby
test "ignores comments from bot users" do
  payload = {
    comment: {
      body: "@trybotster please help",
      user: { login: "trybotster" }  # Bot user
    },
    ...
  }

  assert_no_difference "Bot::Message.count" do
    post "/github/webhooks", ...
  end
end
```

---

## Testing Best Practices

### 1. Use Descriptive Test Names

```ruby
# Good
test "issue_comment on PR with linked issue routes to issue"

# Bad
test "routing works"
```

### 2. Test One Thing Per Test

```ruby
# Good - focused test
test "extract_linked_issues finds Fixes references" do
  issues = controller.send(:extract_linked_issues, "Fixes #123")
  assert_equal [123], issues
end

# Bad - testing multiple things
test "extract_linked_issues works" do
  # Tests Fixes, Closes, Resolves, case sensitivity, duplicates...
end
```

### 3. Use Setup/Teardown for Common Code

```ruby
setup do
  @user = users(:one)
  @webhook_secret = "test_secret"
end
```

### 4. Avoid Test Interdependence

```ruby
# Good - self-contained
test "creates message" do
  assert_difference "Bot::Message.count", 1 do
    Bot::Message.create!(event_type: "test", payload: {})
  end
end

# Bad - depends on other tests
test "updates message created in previous test" do
  message = Bot::Message.last  # Assumes previous test ran
  message.update!(payload: { updated: true })
end
```

### 5. Skip Tests That Require External Dependencies

```ruby
test "fetches PR from GitHub API" do
  skip "Requires GitHub API access - tested manually"

  # Test code that would call real GitHub API
end
```

### 6. Clean Up Debug Output

```ruby
# Remove debug puts/prints before committing
# Good for temporary debugging:
unless response.successful?
  puts "\nResponse status: #{response.status}"
  puts "Response body: #{response.body}"
end

# But remove before committing tests
```

---

## Related Documentation

- [Webhook Implementation Guide](webhook-implementation.md) - Detailed webhook routing documentation
- [Controller Patterns](routing-and-controllers.md) - Controller best practices
- [Database Patterns](database-patterns.md) - ActiveRecord and database testing

---

**Testing Philosophy:**

> Write tests to prevent regressions, not to hit coverage targets.
> Focus on testing behavior, not implementation details.
> A failing test should tell you exactly what broke.
