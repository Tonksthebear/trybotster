//! Timeout-enforcing `pcall` variant for hook dispatch.
//!
//! Exposes `__hook_timed_pcall(func, timeout_ms, ...)` as a Lua global.
//! Semantics match Lua's `pcall`: returns `(true, ...values)` on success,
//! `(false, err_message)` on error. The difference is a per-call deadline —
//! if the callee runs longer than `timeout_ms` ms (wall clock), a Lua VM
//! hook raises a runtime error that falls back out as a regular pcall error.
//!
//! Used by `cli/lua/hub/hooks.lua` to prevent a runaway interceptor from
//! wedging the Hub event loop. Implementing the hook in Rust means the
//! Lua `debug` standard library does not have to be loaded in the VM.
//!
//! # Nested calls
//!
//! mlua's `set_hook` replaces rather than stacks. To make nested
//! `hooks.call` invocations safe, deadlines are tracked in a thread-local
//! stack and the VM hook reads the tightest (most-recent) deadline each
//! time it fires. The hook is installed once per outermost call and
//! removed when the stack empties. This keeps the outer call's deadline
//! enforced when an inner nested call finishes.
//!
//! # LuaJIT and JIT-compiled traces
//!
//! LuaJIT's count hooks are not fired inside JIT-compiled traces — once
//! a tight loop like `while true do end` has been compiled to a trace,
//! it runs as native code and the deadline hook never fires. To keep
//! the watchdog effective under LuaJIT we mark the callback (and
//! sub-functions it calls) as `jit.off(func, true)` before each call,
//! which persistently excludes them from tracing. This keeps the rest
//! of the VM JIT-enabled; only interceptor bodies pay the deopt cost,
//! and only once per function (subsequent `jit.off` calls are no-ops).
//! The `jit` table does not exist on PUC Lua, so the toggle is a no-op
//! there.
//!
//! # Limitations
//!
//! The watchdog fires only on Lua VM instruction boundaries. A callback
//! blocked inside a Rust primitive (e.g., a synchronous HTTP call) burns
//! no Lua instructions and will not time out — the shutdown watchdog in
//! `cli/src/shutdown.rs` is the backstop for those cases.
//
// Rust guideline compliant 2026-04

use std::cell::RefCell;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use mlua::{Function, HookTriggers, IntoLuaMulti, Lua, MultiValue, Value, VmState};

/// Fire the deadline check every N Lua VM instructions.
///
/// 10,000 is a compromise: at a typical VM speed of ~10M–100M instructions/sec,
/// the check runs every 100μs–1ms, which is fine-grained enough to detect a
/// tight infinite loop well within the default 10ms interceptor budget while
/// adding negligible overhead to fast-path hooks.
const INSTRUCTIONS_PER_CHECK: u32 = 10_000;

thread_local! {
    /// Stack of (deadline, timeout_ms) entries — one per active `hook_timed_pcall`
    /// frame on this thread. The VM hook reads `last()` to find the current
    /// (innermost) deadline. Nested frames share the same installed hook.
    static DEADLINE_STACK: RefCell<Vec<(Instant, u64)>> = const { RefCell::new(Vec::new()) };
}

/// Register `__hook_timed_pcall` as a Lua global.
///
/// # Errors
///
/// Returns an error if registering the function with the Lua state fails.
pub fn register(lua: &Lua) -> Result<()> {
    let f = lua
        .create_function(hook_timed_pcall)
        .map_err(|e| anyhow!("create __hook_timed_pcall: {e}"))?;
    lua.globals()
        .set("__hook_timed_pcall", f)
        .map_err(|e| anyhow!("set __hook_timed_pcall global: {e}"))?;
    Ok(())
}

/// The primitive body. Signature mirrors `pcall` with `timeout_ms` injected
/// between the function and its arguments:
///
/// ```lua
/// local ok, a, b, c = __hook_timed_pcall(fn, 50, arg1, arg2)
/// ```
fn hook_timed_pcall(
    lua: &Lua,
    (func, timeout_ms, args): (Function, u64, MultiValue),
) -> mlua::Result<MultiValue> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);

    // Push our deadline onto the thread-local stack and decide whether we
    // are the outermost frame — the outermost frame owns the VM hook's
    // lifetime. Nested frames inherit the already-installed hook; the hook
    // itself always reads the stack top, so the tightest deadline wins.
    let was_outermost = DEADLINE_STACK.with(|stack| {
        let mut stack = stack.borrow_mut();
        let empty = stack.is_empty();
        stack.push((deadline, timeout_ms));
        empty
    });

    if was_outermost {
        let triggers = HookTriggers::new().every_nth_instruction(INSTRUCTIONS_PER_CHECK);
        lua.set_hook(triggers, |_lua, _debug| {
            DEADLINE_STACK.with(|stack| {
                let stack = stack.borrow();
                if let Some(&(deadline, timeout_ms)) = stack.last() {
                    if Instant::now() > deadline {
                        return Err(mlua::Error::RuntimeError(format!(
                            "hook timeout: {timeout_ms}ms exceeded"
                        )));
                    }
                }
                Ok(VmState::Continue)
            })
        });
    }

    // LuaJIT: persistently disable JIT tracing for this callback and any
    // sub-functions it calls, so count hooks fire inside what would
    // otherwise be compiled traces. Applied on every call (not just the
    // outermost) to catch nested closures whose prototypes aren't reached
    // by the outer recursive flag. Idempotent — second+ calls are no-ops.
    // Scoped per-function so the rest of the VM stays JIT-enabled. No-op
    // on PUC Lua where `jit` is nil.
    let _ = lua
        .load(r"if jit and jit.off then jit.off(..., true) end")
        .call::<()>(func.clone());

    // RAII guard ensures the stack entry is popped and (if outermost) the
    // VM hook is removed even if `func.call` unwinds. Without this, a panic
    // inside Lua → Rust transitions could leak the installed hook into the
    // next hook_timed_pcall call and cause spurious timeouts.
    //
    // Scoped to drop immediately after `func.call` so post-processing (result
    // packing, error string creation) runs with the hook already cleared.
    struct Guard<'a> {
        lua: &'a Lua,
        remove_hook_on_drop: bool,
    }
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            DEADLINE_STACK.with(|stack| {
                stack.borrow_mut().pop();
            });
            if self.remove_hook_on_drop {
                self.lua.remove_hook();
            }
        }
    }

    let call_result: mlua::Result<MultiValue> = {
        let _guard = Guard {
            lua,
            remove_hook_on_drop: was_outermost,
        };
        func.call(args)
    };

    match call_result {
        Ok(values) => {
            // Prepend `true` to match `pcall` semantics.
            let mut out: Vec<Value> = Vec::with_capacity(values.len() + 1);
            out.push(Value::Boolean(true));
            for v in values {
                out.push(v);
            }
            Ok(MultiValue::from_iter(out))
        }
        Err(e) => {
            let err_str = lua.create_string(e.to_string())?;
            (false, Value::String(err_str)).into_lua_multi(lua)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A runaway interceptor aborts after its own deadline, leaving the Rust
    /// thread-local stack empty so the next call is not poisoned.
    #[test]
    fn deadline_stack_clears_after_timeout() {
        let lua = Lua::new();
        register(&lua).expect("register primitive");

        // Runaway loop with tight deadline. __hook_timed_pcall returns
        // (false, err) rather than propagating.
        let (ok, _err): (bool, mlua::Value) = lua
            .load(
                r#"
                return __hook_timed_pcall(function()
                    while true do end
                end, 25)
            "#,
            )
            .eval()
            .expect("call returns even though the callee loops forever");

        assert!(!ok, "runaway call should surface as a pcall-style error");
        DEADLINE_STACK.with(|stack| {
            assert!(
                stack.borrow().is_empty(),
                "deadline stack must be empty after call returns"
            );
        });
    }

    /// A nested call uses the tightest (inner) deadline, times out, and the
    /// outer frame continues. The outer deadline is still enforced on resume.
    #[test]
    fn nested_call_inner_timeout_does_not_leak_hook() {
        let lua = Lua::new();
        register(&lua).expect("register primitive");

        // Outer: generous deadline. Inner: tight, loops forever.
        let result: i64 = lua
            .load(
                r#"
                local outer_ok, outer_result = __hook_timed_pcall(function()
                    local inner_ok, inner_err = __hook_timed_pcall(function()
                        while true do end
                    end, 20)
                    assert(not inner_ok, "inner should time out")
                    -- outer still has budget; return a sentinel.
                    return 42
                end, 5000)
                assert(outer_ok, "outer should succeed: " .. tostring(outer_result))
                return outer_result
            "#,
            )
            .eval()
            .expect("nested call returns outer result");

        assert_eq!(result, 42);
        DEADLINE_STACK.with(|stack| {
            assert!(
                stack.borrow().is_empty(),
                "deadline stack must be empty after nested call unwinds"
            );
        });
    }

    /// If the outer frame has already exhausted its deadline during the
    /// inner call, the next hook-fire in the outer frame must raise.
    #[test]
    fn outer_deadline_enforced_after_inner_returns() {
        let lua = Lua::new();
        register(&lua).expect("register primitive");

        // Outer has a tight deadline; inner is fast. The outer then burns
        // time past its deadline; its hook must fire.
        let (ok, _err): (bool, mlua::Value) = lua
            .load(
                r#"
                return __hook_timed_pcall(function()
                    -- Fast inner returns quickly.
                    local inner_ok = __hook_timed_pcall(function() return 1 end, 1000)
                    assert(inner_ok, "inner should succeed")
                    -- Now burn time past outer's 30ms deadline.
                    while true do end
                end, 30)
            "#,
            )
            .eval()
            .expect("outer call returns on timeout");

        assert!(!ok, "outer deadline must still fire after inner completes");
    }

    /// Regression test for LuaJIT: after the callback has been hot-traced
    /// by the JIT compiler, the deadline hook must still fire inside its
    /// compiled trace. Without the per-function `jit.off(..., true)` the
    /// compiled trace runs as native code and skips count hooks, which
    /// caused the CI `cli-test` job to hang indefinitely on the first
    /// LuaJIT build. On PUC Lua this is a no-op (there is no `jit`
    /// table) and the test degrades to the same guarantee as
    /// `deadline_stack_clears_after_timeout`.
    #[test]
    fn callback_previously_hot_traced_still_times_out() {
        let lua = Lua::new();
        register(&lua).expect("register primitive");

        // Call the same callback enough times that LuaJIT decides it is
        // hot and traces it — the default hotloop threshold is ~56
        // iterations. Each successful call returns `1`, so the trace is
        // for the early-return path. Then call it one more time with the
        // runaway path enabled: if the hook is skipped inside the trace,
        // the test hangs forever; if the fix is in place, the deadline
        // aborts as expected.
        let (ok, _err): (bool, mlua::Value) = lua
            .load(
                r#"
                local function body(run_forever)
                    if run_forever then
                        while true do end
                    end
                    return 1
                end
                for _ = 1, 500 do
                    local inner_ok = __hook_timed_pcall(body, 1000, false)
                    assert(inner_ok, "warmup iteration should succeed")
                end
                return __hook_timed_pcall(body, 25, true)
            "#,
            )
            .eval()
            .expect("call returns even after the body has been hot-traced");

        assert!(
            !ok,
            "deadline hook must fire inside a JIT-compiled trace (or a no-op on PUC Lua)"
        );
    }
}
