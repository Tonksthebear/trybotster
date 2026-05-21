//! Rust-hosted Lua tests for MCP proxy behavior.

use mlua::Lua;

fn create_lua_vm() -> Lua {
    let lua = Lua::new();

    botster::lua::primitives::json::register(&lua).expect("json register");
    botster::lua::primitives::log::register(&lua).expect("log register");

    let lua_dir = std::env::current_dir()
        .unwrap()
        .join("lua")
        .to_str()
        .unwrap()
        .to_string();
    lua.load(format!(
        r#"package.path = "{lua_dir}/?.lua;{lua_dir}/?/init.lua;" .. package.path"#
    ))
    .exec()
    .expect("set package.path");

    lua.load(
        r#"
        _G.hub = { is_offline = function() return false end }
        _G.events = { emit = function() return true end }
        _G.timer = {
          cancel = function() return true end,
          after = function(_, callback)
            callback()
            return "timer"
          end,
        }
        "#,
    )
    .exec()
    .expect("stub hub/events/timer");

    lua
}

#[test]
fn proxied_tool_call_forwards_botster_context_in_meta() {
    let lua = create_lua_vm();

    let ok: bool = lua
        .load(
            r#"
            _G.requests = {}
            _G.http = {
              request = function(req, callback)
                table.insert(_G.requests, req)
                local decoded = json.decode(req.body)

                if decoded.method == "initialize" then
                  callback({
                    status = 200,
                    headers = { ["Mcp-Session-Id"] = "remote-session" },
                    body = json.encode({ jsonrpc = "2.0", id = decoded.id, result = {} }),
                  }, nil)
                elseif decoded.method == "tools/list" then
                  callback({
                    status = 200,
                    headers = {},
                    body = json.encode({
                      jsonrpc = "2.0",
                      id = decoded.id,
                      result = {
                        tools = {
                          {
                            name = "remote_tool",
                            description = "Remote tool",
                            inputSchema = { type = "object", properties = {} },
                          },
                        },
                      },
                    }),
                  }, nil)
                elseif decoded.method == "tools/call" then
                  _G.forwarded_call = decoded
                  callback({
                    status = 200,
                    headers = {},
                    body = json.encode({
                      jsonrpc = "2.0",
                      id = decoded.id,
                      result = {
                        content = { { type = "text", text = "ok" } },
                        isError = false,
                      },
                    }),
                  }, nil)
                else
                  error("unexpected method: " .. tostring(decoded.method))
                end
              end,
            }

            local mcp = require("lib.mcp")
            mcp.proxy("https://example.test/mcp")
            mcp.call_tool("remote_tool", { value = 1 }, {
              session_uuid = "sess-1234567890abcdef",
              agent_name = "codex",
              session_name = "codex",
              branch_name = "realignment",
              repo = "owner/repo",
              workspace_id = "ws-123",
              worktree_path = "/tmp/worktree",
            }, function(content, err)
              _G.callback_err = err
              _G.callback_text = content and content[1] and content[1].text
            end)

            local params = _G.forwarded_call.params
            local meta = params and params._meta and params._meta.botster
            return _G.callback_err == nil
              and _G.callback_text == "ok"
              and params.name == "remote_tool"
              and params.arguments.value == 1
              and meta.session_uuid == "sess-1234567890abcdef"
              and meta.agent_name == "codex"
              and meta.session_name == "codex"
              and meta.branch_name == "realignment"
              and meta.repo == "owner/repo"
              and meta.workspace_id == "ws-123"
              and meta.worktree_path == "/tmp/worktree"
            "#,
        )
        .eval()
        .expect("proxied tool call should forward botster metadata");

    assert!(
        ok,
        "proxied MCP tools/call should include _meta.botster context"
    );
}

#[test]
fn proxied_tool_call_applies_generic_argument_transform() {
    let lua = create_lua_vm();

    let ok: bool = lua
        .load(
            r#"
            _G.http = {
              request = function(req, callback)
                local decoded = json.decode(req.body)

                if decoded.method == "initialize" then
                  callback({
                    status = 200,
                    headers = { ["Mcp-Session-Id"] = "remote-session" },
                    body = json.encode({ jsonrpc = "2.0", id = decoded.id, result = {} }),
                  }, nil)
                elseif decoded.method == "tools/list" then
                  callback({
                    status = 200,
                    headers = {},
                    body = json.encode({
                      jsonrpc = "2.0",
                      id = decoded.id,
                      result = {
                        tools = {
                          {
                            name = "remote_tool",
                            description = "Remote tool",
                            inputSchema = { type = "object", properties = {} },
                          },
                        },
                      },
                    }),
                  }, nil)
                elseif decoded.method == "tools/call" then
                  _G.forwarded_call = decoded
                  callback({
                    status = 200,
                    headers = {},
                    body = json.encode({
                      jsonrpc = "2.0",
                      id = decoded.id,
                      result = {
                        content = { { type = "text", text = "ok" } },
                        isError = false,
                      },
                    }),
                  }, nil)
                else
                  error("unexpected method: " .. tostring(decoded.method))
                end
              end,
            }

            local mcp = require("lib.mcp")
            mcp.proxy("https://example.test/mcp", {
              transform_arguments = function(name, arguments)
                if name == "remote_tool" and arguments.flag == false then
                  local out = {}
                  for key, value in pairs(arguments) do
                    out[key] = value
                  end
                  out.flag = nil
                  return out
                end
                return arguments
              end,
            })
            mcp.call_tool("remote_tool", { value = 1, flag = false }, {}, function(content, err)
              _G.callback_err = err
              _G.callback_text = content and content[1] and content[1].text
            end)

            local arguments = _G.forwarded_call.params.arguments
            return _G.callback_err == nil
              and _G.callback_text == "ok"
              and arguments.value == 1
              and arguments.flag == nil
            "#,
        )
        .eval()
        .expect("proxied tool call should apply argument transform");

    assert!(
        ok,
        "proxied MCP tools/call should apply generic argument transform before forwarding"
    );
}
