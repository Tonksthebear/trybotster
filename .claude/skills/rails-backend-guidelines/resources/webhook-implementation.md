# Webhook Implementation Guide - GitHub Webhooks and PR-to-Issue Routing

Complete guide to the GitHub webhook system, including PR-to-issue routing and structured context formatting.

## Table of Contents

- [Overview](#overview)
- [Webhook Flow](#webhook-flow)
- [PR-to-Issue Routing](#pr-to-issue-routing)
- [Structured Context Format](#structured-context-format)
- [Implementation Details](#implementation-details)
- [Testing Webhooks](#testing-webhooks)

---

## Overview

The webhook system processes GitHub events (issue comments, PR comments) and routes @trybotster mentions to the appropriate agent. The key feature is **PR-to-issue routing**: when a PR comment mentions @trybotster and the PR is linked to an issue, the comment routes to the original issue's agent instead of creating a new PR agent.

### Key Concepts

- **Webhook Signature Verification**: All GitHub webhooks must have valid HMAC signatures
- **@trybotster Mention Detection**: Only comments mentioning @trybotster are processed
- **Bot Comment Filtering**: Comments from bot users are ignored to prevent loops
- **PR-to-Issue Routing**: PR comments route to linked issues when applicable
- **Structured Context**: AI agents receive formatted context with clear routing information

---

## Webhook Flow

### Request Lifecycle

```
1. GitHub sends webhook POST to /github/webhooks
   ↓
2. verify_github_signature! validates HMAC signature
   ↓
3. parse_webhook_payload converts JSON to hash
   ↓
4. Event routing based on X-GitHub-Event header:
   - issue_comment → handle_issue_comment
   - pull_request_review_comment → handle_pr_review_comment
   ↓
5. Check for @trybotster mention and filter bot users
   ↓
6. Determine if PR is linked to an issue
   ↓
7. Build structured context
   ↓
8. Create Integrations::Github::Message with formatted prompt
   ↓
9. Agent processes the message
```

### Event Types Supported

```ruby
# Issue or PR comment (both use the same event)
X-GitHub-Event: issue_comment
Actions: created, edited

# PR review comment (inline code comments)
X-GitHub-Event: pull_request_review_comment
Actions: created, edited
```

---

## PR-to-Issue Routing

### How It Works

When a user comments on a PR that's linked to an issue (via "Fixes #123", "Closes #456", etc.), the system routes the comment back to the original issue's agent rather than creating a new PR agent.

### Linking Patterns Detected

```ruby
# Supported patterns (case-insensitive):
- Fixes #123
- Fixed #123
- Fix #123
- Closes #123
- Closed #123
- Close #123
- Resolves #123
- Resolved #123
- Resolve #123
- References #123
- Referenced #123
```

### Implementation

```ruby
# app/controllers/github/webhooks_controller.rb

# Extract linked issues from PR body
def extract_linked_issues(pr_body)
  return [] if pr_body.blank?
  
  pattern = /(?:fix(?:es|ed)?|close(?:s|d)?|resolve(?:s|d)?|references?)\s+#(\d+)/i
  matches = pr_body.scan(pattern)
  matches.flatten.map(&:to_i).uniq
end

# Fetch PR details and find linked issue
def fetch_linked_issue_for_pr(repo_full_name, pr_number)
  installation_result = Github::App.get_installation_for_repo(repo_full_name)
  return nil unless installation_result[:success]

  token_result = Github::App.get_installation_token(installation_result[:installation_id])
  return nil unless token_result[:success]

  client = Github::App.client(token_result[:token])
  pr = client.pull_request(repo_full_name, pr_number)
  
  linked_issues = extract_linked_issues(pr.body)
  linked_issues.first
rescue StandardError => e
  Rails.logger.warn "Could not fetch PR: #{e.message}"
  nil
end

# In handle_issue_comment
if is_pr
  linked_issue = fetch_linked_issue_for_pr(repo_full_name, issue_number)
  
  if linked_issue
    # Route to the linked issue
    pr_number = issue_number
    routed_info = {
      source_number: issue_number,
      source_type: "pr",
      target_number: linked_issue,
      target_type: "issue",
      reason: "pr_linked_to_issue"
    }
    target_issue_number = linked_issue
    is_pr = false
  end
end
```

### Example Routing Scenario

```
User creates Issue #720: "Add webhook routing tests"
  ↓
Agent creates PR #731 with description: "Fixes #720"
  ↓
User comments on PR #731: "@trybotster Were there no tests needed?"
  ↓
System detects PR #731 links to Issue #720
  ↓
Comment routes to Issue #720's agent (not creating new PR #731 agent)
  ↓
Agent responds on PR #731 with context from Issue #720
```

---

## Structured Context Format

### Overview

The system provides agents with structured context that clearly indicates:
- Where the question came from (source)
- Where it was routed to (routed_to)
- Where to respond (respond_to)
- What the actual question is (message)
- What task to perform (task)
- Special requirements (requirements)

### Data Structure

```ruby
# When routing from PR to issue
{
  source: {
    type: "pr_comment",           # Type of source
    repo: "owner/repo",            # Repository
    owner: "owner",                # Owner
    repo_name: "repo",             # Repo name
    number: 731,                   # PR number
    comment_author: "username"     # Who asked
  },
  routed_to: {
    type: "issue",                 # Where routed
    number: 720,                   # Issue number
    reason: "pr_linked_to_issue"   # Why routed
  },
  respond_to: {
    type: "pr",                    # Where to respond
    number: 731,                   # PR number
    instruction: "Post your response as a comment on PR #731"
  },
  message: "@trybotster Were there no tests needed?",
  task: "Answer the question about the PR changes",
  requirements: {
    must_use_trybotster_mcp: true,
    fetch_first: "pr",             # Fetch PR first
    number_to_fetch: 731,          # PR to fetch
    context_number: 720            # Issue for context
  }
}
```

### Formatted Output

The structured context is formatted into a human-readable prompt:

```markdown
## Source
Type: pr_comment
Repository: owner/repo (owner/repo)
Number: #731
Author: username

## Routing
This comment was made on PR #731
Routed to: issue #720
Reason: pr_linked_to_issue

## Message
@trybotster Were there no tests needed?

## Where to Respond
You must respond on: PR #731
Post your response as a comment on PR #731

## Your Task
Answer the question about the PR changes

## Requirements
- You MUST use ONLY the trybotster MCP server to interact with GitHub
- Start by fetching pr #731 details using the MCP server
- You may fetch issue #720 for additional context if needed
- After fetching, you'll have full context to answer the question
- Post your response on pr #731
```

### Implementation

```ruby
# Build the structured context
def build_structured_context(repo:, issue_number:, is_pr:, comment_body:, 
                              comment_author:, source_type: nil, routed_info: nil)
  owner, repo_name = repo.split("/")
  
  if routed_info
    # Routing from PR to issue or vice versa
    {
      source: {
        type: source_type,
        repo: repo,
        owner: owner,
        repo_name: repo_name,
        number: routed_info[:source_number],
        comment_author: comment_author
      },
      routed_to: {
        type: routed_info[:target_type],
        number: routed_info[:target_number],
        reason: routed_info[:reason]
      },
      respond_to: {
        type: routed_info[:source_type],
        number: routed_info[:source_number],
        instruction: "Post your response as a comment on #{routed_info[:source_type].upcase} ##{routed_info[:source_number]}"
      },
      message: comment_body,
      task: "Answer the question about the #{routed_info[:source_type].upcase} changes",
      requirements: {
        must_use_trybotster_mcp: true,
        fetch_first: routed_info[:source_type],
        number_to_fetch: routed_info[:source_number],
        context_number: routed_info[:target_number]
      }
    }
  else
    # Direct mention (no routing)
    {
      source: {
        type: is_pr ? "pr_comment" : "issue_comment",
        repo: repo,
        owner: owner,
        repo_name: repo_name,
        number: issue_number,
        comment_author: comment_author
      },
      message: comment_body,
      task: "Answer the question or help with the #{is_pr ? 'PR' : 'issue'}",
      requirements: {
        must_use_trybotster_mcp: true,
        fetch_first: is_pr ? "pr" : "issue",
        number_to_fetch: issue_number
      }
    }
  end
end

# Format the structured context for the AI
def format_structured_context(ctx)
  sections = []
  
  # Source section
  sections << "## Source"
  sections << "Type: #{ctx[:source][:type]}"
  sections << "Repository: #{ctx[:source][:repo]} (#{ctx[:source][:owner]}/#{ctx[:source][:repo_name]})"
  sections << "Number: ##{ctx[:source][:number]}"
  sections << "Author: #{ctx[:source][:comment_author]}"
  
  # Routing section (if routed)
  if ctx[:routed_to]
    sections << ""
    sections << "## Routing"
    sections << "This comment was made on #{ctx[:source][:type].gsub('_', ' ')}"
    sections << "Routed to: #{ctx[:routed_to][:type]} ##{ctx[:routed_to][:number]}"
    sections << "Reason: #{ctx[:routed_to][:reason]}"
  end
  
  # Message section
  sections << ""
  sections << "## Message"
  sections << ctx[:message]
  
  # Response instructions (if routed)
  if ctx[:respond_to]
    sections << ""
    sections << "## Where to Respond"
    sections << "You must respond on: #{ctx[:respond_to][:type].upcase} ##{ctx[:respond_to][:number]}"
    sections << ctx[:respond_to][:instruction]
  end
  
  # Task section
  sections << ""
  sections << "## Your Task"
  sections << ctx[:task]
  
  # Requirements section
  sections << ""
  sections << "## Requirements"
  sections << "- You MUST use ONLY the trybotster MCP server to interact with GitHub"
  
  if ctx[:requirements][:fetch_first]
    sections << "- Start by fetching #{ctx[:requirements][:fetch_first]} ##{ctx[:requirements][:number_to_fetch]} details using the MCP server"
  end
  
  if ctx[:requirements][:context_number]
    sections << "- You may fetch #{ctx[:routed_to][:type]} ##{ctx[:requirements][:context_number]} for additional context if needed"
  end
  
  sections << "- After fetching, you'll have full context to answer the question"
  
  if ctx[:respond_to]
    sections << "- Post your response on #{ctx[:respond_to][:type]} ##{ctx[:respond_to][:number]}"
  end
  
  sections.join("\n")
end
```

---

## Implementation Details

### Integrations::Github::Message Payload Structure

The message stores `repo` and `issue_number` as top-level columns; the payload holds structured context:

```ruby
message = Integrations::Github::Message.create!(
  event_type: "github_mention",
  repo: repo,
  issue_number: issue_number,
  payload: {
    prompt: formatted_context,
    structured_context: structured_context,
    comment_id: comment_id,
    comment_body: comment_body,
    comment_author: comment_author,
    issue_title: issue_title,
    issue_body: issue_body,
    issue_url: issue_url,
    is_pr: is_pr,
    installation_id: installation_id
  }
)
```

### Key Fields

- **`prompt`**: Complete formatted prompt ready for AI consumption (used by botster-hub)
- **`structured_context`**: Hash with structured routing information
- **Raw fields**: Individual data points for custom prompt construction
- **`context`**: Legacy field, same as `prompt` for backwards compatibility

### Webhook Secret Configuration

```ruby
# Production/Development
ENV["GITHUB_WEBHOOK_SECRET"] = "your-secret-here"

# Test environment
setup do
  ENV["GITHUB_WEBHOOK_SECRET"] = "test_secret"
end

teardown do
  ENV.delete("GITHUB_WEBHOOK_SECRET")
end
```

---

## Testing Webhooks

### Test Structure

```ruby
test "issue_comment on PR with linked issue routes to issue" do
  # 1. Create payload simulating GitHub webhook
  payload = {
    action: "created",
    issue: {
      number: 200,
      pull_request: { url: "..." },  # Indicates it's a PR
      body: "Fixes #50"  # Links to issue #50
    },
    comment: {
      body: "@trybotster update needed",
      user: { login: "commenter" }
    },
    repository: { full_name: "test/repo" }
  }
  
  # 2. Sign the payload
  body, signature = sign_webhook_payload(payload)
  
  # 3. Send webhook request
  post "/github/webhooks",
    params: body,
    headers: {
      "Content-Type" => "application/json",
      "X-GitHub-Event" => "issue_comment",
      "X-Hub-Signature-256" => signature
    }
  
  # 4. Verify routing
  message = Integrations::Github::Message.last
  assert_equal 50, message.issue_number, "Should route to issue #50"
  assert_equal false, message.payload["is_pr"], "Should be marked as issue"
end
```

### Helper Method for Signing

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
```

### Common Test Scenarios

```ruby
# Test 1: Direct issue comment
test "issue_comment creates bot message"

# Test 2: PR comment without linked issue
test "PR comment creates PR bot message"

# Test 3: PR comment with linked issue (routing)
test "PR comment routes to linked issue"

# Test 4: Bot comment filtering
test "ignores comments from bot users"

# Test 5: Missing @trybotster mention
test "ignores comments without @trybotster"

# Test 6: Edited comments
test "processes edited comments with @trybotster"

# Test 7: Extract linked issues
test "extract_linked_issues finds Fixes references"
test "extract_linked_issues handles case insensitive"
test "extract_linked_issues removes duplicates"
```

### Skipping Tests That Require External APIs

```ruby
test "fetches PR from GitHub API" do
  skip "Requires GitHub API access - tested via extract_linked_issues unit tests"
  
  # Test code that would require real GitHub API access
  # Instead, test the parsing logic separately
end
```

---

## Architecture Decisions

### Why Both `prompt` and `structured_context`?

1. **`prompt`**: Optimized for botster-hub consumption - complete, formatted, ready to use
2. **`structured_context`**: Allows programmatic access to routing metadata for debugging and custom processing
3. **Raw fields**: Enables custom prompt construction if needed in the future

### Why Route PR Comments to Issues?

1. **Maintains Context**: The agent has full context of the original problem
2. **Prevents Duplication**: Avoids creating redundant PR-specific agents
3. **Improves Conversations**: Keeps the entire conversation (issue + PR + reviews) connected
4. **Better Decisions**: Agent can reference original requirements when reviewing PR

### Why Format in Rails (Not Rust Hub)?

1. **Rails has full context**: Has access to GitHub API, database, etc.
2. **Easier to test**: Can test formatting logic in Rails tests
3. **Hub stays simple**: Hub just passes through the `prompt` field
4. **Flexibility**: Rails can adjust format without hub changes

---

## Related Files

- **Controller**: `app/controllers/github/webhooks_controller.rb`
- **Tests**: `test/controllers/github/webhooks_controller_test.rb`
- **Routes**: `config/routes.rb` (POST /github/webhooks)
- **GitHub App**: `app/models/github/app.rb`
- **Bot Message**: `app/models/bot/message.rb`

---

## Troubleshooting

### Webhook Not Triggering

1. Check signature verification: `GITHUB_WEBHOOK_SECRET` must be set
2. Check event type: Only `issue_comment` and `pull_request_review_comment` are handled
3. Check action: Only `created` and `edited` actions are processed
4. Check mention: Comment must include `@trybotster`
5. Check author: Comments from bot users are filtered

### Routing Not Working

1. Verify PR has linked issue in body: "Fixes #123", "Closes #456", etc.
2. Check GitHub API access: Installation token must be valid
3. Check logs: Look for "Could not fetch PR" warnings
4. Test extraction: Use `extract_linked_issues` unit tests

### Tests Failing

1. Check route: Use `/github/webhooks` not `/webhooks/github`
2. Check signature: Set `ENV["GITHUB_WEBHOOK_SECRET"]` in setup
3. Check fixtures: Only load fixtures you need
4. Check database: Run `RAILS_ENV=test bin/rails db:reset` if needed

---

**Key Takeaway:**

> The webhook system is designed to maintain conversation context by routing PR comments back to their originating issues, while providing clear, structured context to AI agents about where questions came from and where to respond.
