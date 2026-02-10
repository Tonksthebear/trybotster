# Testing Guide — Botster Rails

Vanilla Rails. Minitest. Fixtures. No RSpec. No FactoryBot.

## Running Tests

```bash
bin/rails test                                    # all tests
bin/rails test test/models/hub_test.rb            # one file
bin/rails test test/models/hub_test.rb -n test_valid_hub  # one test
bin/rails test --verbose                          # verbose output
```

**Never** use `bin/rails test path/to/test.rb::test_name` — use `-n test_name` flag.

**Rust CLI tests** (separate from Rails):
```bash
cd cli && ./test.sh          # all tests
cd cli && ./test.sh --unit   # unit only
cd cli && ./test.sh -- scroll  # matching pattern
```
Always use `./test.sh`, never raw `cargo test` (needs `BOTSTER_ENV=test` for keyring bypass).

---

## Test Types & Organization

```
test/
├── models/          # ActiveSupport::TestCase — validations, scopes, methods
├── controllers/     # ActionDispatch::IntegrationTest — routes, auth, render
├── requests/        # ActionDispatch::IntegrationTest — API contract tests
├── integration/     # CliIntegrationTestCase — real CLI binary + real server
├── system/          # ApplicationSystemTestCase — Selenium + real CLI
├── jobs/            # ActiveSupport::TestCase — background jobs
├── mcp/             # Domain-specific (MCP tools, identifiers)
├── support/         # Helpers included by test types
└── fixtures/        # YAML fixtures
```

### Model tests (`ActiveSupport::TestCase`)
Standard transactional tests. Use fixtures. Test validations, scopes, instance methods.

### Request tests (`ActionDispatch::IntegrationTest`)
Test API endpoints the CLI calls. Include `ApiTestHelper` for auth. Transactional.

### CLI integration tests (`CliIntegrationTestCase`)
Spawn a real `botster-hub` binary against a real Puma server. **Non-transactional** because the CLI process is a separate OS process that needs to see committed data. Inherit from `CliIntegrationTestCase`.

### System tests (`ApplicationSystemTestCase`)
Selenium + headless Chrome + real CLI. **Non-transactional**. Use Warden test helpers for auth.

---

## Fixtures

All fixtures in `test/fixtures/`. Loaded globally via `fixtures :all` in `test_helper.rb`.

### Key fixtures

```yaml
# users.yml — :jason is the primary test user
jason:
  email: jason@example.com
  username: jason
  provider: github
  uid: "11111"

# hubs.yml — named fixtures for different states
active_hub:
  user: jason
  device: cli_device
  identifier: "hub-active-123"
  last_seen_at: <%= 30.seconds.ago %>
  alive: true

stale_hub:
  user: jason
  device: cli_device
  identifier: "hub-stale-456"
  last_seen_at: <%= 5.minutes.ago %>
  alive: false

# devices.yml — device + token pair
cli_device:
  user: jason
  device_type: cli
  name: Test CLI Device
  fingerprint: "aa:bb:cc:dd:ee:ff:00:11"

# device_tokens.yml
cli_device_token:
  device: cli_device
  name: "CLI Device Token"
  token: "btstr_test_token_cli_device_12345678"
```

### Fixture conventions

- Name fixtures by state/role: `active_hub`, `stale_hub`, `cli_device`
- Use ERB for time: `<%= 30.seconds.ago %>`
- Reference other fixtures by name: `user: jason`, `device: cli_device`
- If a model has `set_fixture_class`, it's in `test_helper.rb`:
  ```ruby
  set_fixture_class "integrations/github/mcp_tokens" => Integrations::Github::MCPToken
  ```

### Adding new fixtures

1. Add YAML to `test/fixtures/`
2. Ensure required fields are present (check model validations)
3. Use unique values for uniqueness constraints
4. Reference by name in tests: `users(:jason)`, `hubs(:active_hub)`

---

## Support Helpers

All in `test/support/`, auto-loaded by `test_helper.rb`.

### `ApiTestHelper` — API auth for request tests

```ruby
class HubsControllerTest < ActionDispatch::IntegrationTest
  include ApiTestHelper

  test "creates hub" do
    post hubs_url,
      params: { identifier: "new-hub" }.to_json,
      headers: auth_headers_for(:jason)

    assert_response :created
    json = assert_json_keys(:id, :identifier)
  end
end
```

Key methods:
- `auth_headers_for(:jason)` — creates a device + token dynamically, returns `Authorization: Bearer ...` headers
- `json_headers` — unauthenticated JSON headers
- `assert_json_response` — parse + assert content type
- `assert_json_keys(:id, :name)` — assert keys present
- `assert_json_error("message")` — assert error response

### `WaitHelper` — polling for async assertions

Included in all test cases via `test_helper.rb`.

```ruby
# Raises WaitHelper::TimeoutError after 10s
wait_until(timeout: 10) { hub.reload.last_seen_at.present? }

# Returns boolean, no raise
if wait_until?(timeout: 5) { process_stopped? }
  # ...
end

# With custom error message
wait_until(timeout: 10, message: -> { "Status: #{record.status}" }) { record.done? }
```

### `CliTestHelper` — spawn real CLI binary

Used by `CliIntegrationTestCase` and system tests. Spawns `target/debug/botster-hub` as an OS process.

```ruby
class MyCliTest < CliIntegrationTestCase
  test "CLI registers hub" do
    cli = start_cli(@hub, timeout: 20)

    assert cli.running?
    @hub.reload
    assert @hub.last_seen_at > 5.seconds.ago

    # Access connection URL (written to temp dir by CLI)
    url = cli.connection_url
    assert url.present?
  end
end
```

Key details:
- `start_cli(hub, timeout: 20)` — builds binary if needed, creates temp dir, device token, spawns process
- CLI runs with `BOTSTER_ENV=system_test` (real network, file-based keyring)
- Readiness: polls `hub.last_seen_at` for a heartbeat after `started_at`
- `stop_cli(cli)` — TERM, wait, KILL if needed, cleanup temp dir + device token
- Set `SKIP_CLI_BUILD=1` to skip cargo build
- `CliIntegrationTestCase` auto-tracks started CLIs and stops them in teardown

### `GithubTestHelper` — stub GitHub API

```ruby
include GithubTestHelper

test "something with github" do
  with_stubbed_github do
    # Github::App.get_installation_for_repo, .installation_client,
    # .get_installation_token, .client are all stubbed
    post hub_notifications_url(@hub.identifier), params: {...}
  end
end
```

Uses `define_singleton_method` to temporarily replace class methods, restores originals in `ensure`.

---

## Test Infrastructure

### Parallel tests

```ruby
# test_helper.rb
parallelize(workers: :number_of_processors)

# Stale CLI processes cleaned up ONCE before forking
parallelize_before_fork do
  CliProcessCleanup.cleanup_stale_processes
end
```

### Non-transactional tests

CLI integration and system tests set `self.use_transactional_tests = false` because the CLI is a separate OS process that needs to see committed data. This means:
- Manual cleanup in `teardown`
- `@hub.reload.destroy rescue nil` pattern
- Device tokens cleaned up via `cli.stop`

### WebMock

Enabled globally: `WebMock.disable_net_connect!(allow_localhost: true)`.
Localhost allowed for ActionCable and CLI integration tests.

---

## Writing New Tests

### Model test

```ruby
require "test_helper"

class WidgetTest < ActiveSupport::TestCase
  test "requires name" do
    widget = Widget.new
    assert_not widget.valid?
    assert_includes widget.errors[:name], "can't be blank"
  end

  test "active scope" do
    active = Widget.create!(name: "a", active: true)
    inactive = Widget.create!(name: "b", active: false)
    assert_includes Widget.active, active
    assert_not_includes Widget.active, inactive
  end
end
```

### Request test (API endpoint)

```ruby
require "test_helper"

class ThingsControllerTest < ActionDispatch::IntegrationTest
  include ApiTestHelper

  test "returns 401 without auth" do
    get things_url, headers: json_headers
    assert_response :unauthorized
  end

  test "lists things for authenticated user" do
    get things_url, headers: auth_headers_for(:jason)
    assert_response :ok
    json = assert_json_response
    assert_kind_of Array, json
  end

  test "creates thing" do
    assert_difference -> { Thing.count }, 1 do
      post things_url,
        params: { name: "new" }.to_json,
        headers: auth_headers_for(:jason)
    end
    assert_response :created
  end
end
```

### CLI integration test

```ruby
require_relative "cli_integration_test_case"

class MyFeatureCliTest < CliIntegrationTestCase
  test "CLI does the thing" do
    cli = start_cli(@hub, timeout: 20)
    assert cli.running?

    # Assert through database state
    @hub.reload
    assert @hub.last_seen_at > 1.minute.ago

    # Or through CLI output
    output = cli.recent_output
    assert output.include?("expected string")
  end
end
```

### Webhook test

```ruby
require "test_helper"

module Github
  class WebhooksControllerTest < ActionDispatch::IntegrationTest
    setup do
      @webhook_secret = "test_secret"
      ENV["GITHUB_WEBHOOK_SECRET"] = @webhook_secret
    end

    teardown do
      ENV.delete("GITHUB_WEBHOOK_SECRET")
    end

    test "creates message from issue_comment" do
      payload = {
        action: "created",
        repository: { full_name: "owner/repo" },
        issue: { number: 1, title: "Bug", body: "", html_url: "...", pull_request: nil },
        comment: { id: 1, body: "@trybotster help", user: { login: "someone" } }
      }
      body, signature = sign_webhook_payload(payload)

      assert_difference -> { Integrations::Github::Message.count }, 1 do
        post "/github/webhooks",
          params: body,
          headers: {
            "Content-Type" => "application/json",
            "X-GitHub-Event" => "issue_comment",
            "X-Hub-Signature-256" => signature
          }
      end
      assert_response :success
    end

    private

    def sign_webhook_payload(payload)
      body = payload.to_json
      sig = "sha256=" + OpenSSL::HMAC.hexdigest("sha256", @webhook_secret, body)
      [body, sig]
    end
  end
end
```

---

## Common Assertions

```ruby
assert_response :ok / :created / :unauthorized / :not_found
assert_difference -> { Model.count }, 1 do ... end
assert_no_difference -> { Model.count } do ... end
assert_equal expected, actual
assert_nil / assert_not_nil
assert_includes collection, item
assert_not_includes collection, item
assert_match /regex/, string
assert_raises(ErrorClass) { ... }
assert_operator value, :>, other  # for comparisons
```

---

## Anti-Patterns

- **Don't use FactoryBot** — fixtures only
- **Don't use RSpec** — Minitest only
- **Don't mock when you can use fixtures** — prefer real data
- **Don't test implementation details** — test behavior
- **Don't skip CLI build in CI** — only locally with `SKIP_CLI_BUILD=1`
- **Don't use transactional tests with CLI** — CLI is a separate process
- **Don't precompile assets** — never in this project
