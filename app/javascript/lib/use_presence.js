/**
 * usePresence — Stimulus composable for user presence detection.
 *
 * Tracks two signals:
 *   1. Page visibility (document.visibilitychange)
 *   2. User activity (mouse, keyboard, touch — AFK after timeout)
 *
 * Calls controller.away() when the user is no longer present (page hidden
 * OR idle too long) and controller.back() when they return.
 *
 * Usage:
 *   import { usePresence } from "lib/use_presence";
 *
 *   connect() {
 *     this.teardownPresence = usePresence(this, { ms: 120000 });
 *   }
 *   disconnect() { this.teardownPresence?.(); }
 *   away() { user gone  }
 *   back() { user returned }
 *
 * Returns a teardown function that removes all listeners and clears timers.
 */
export function usePresence(controller, { ms = 120000 } = {}) {
  let pageVisible = !document.hidden;
  let userActive = true;
  let present = true;
  let afkTimer = null;

  const update = () => {
    const now = pageVisible && userActive;
    if (now === present) return;
    present = now;
    now ? controller.back?.() : controller.away?.();
  };

  let lastActivity = 0;
  const onActivity = () => {
    // Throttle: only reset the timer once per second.
    // mousemove fires hundreds of times/sec -- no need to clearTimeout each time.
    const now = Date.now();
    if (now - lastActivity < 1000) return;
    lastActivity = now;

    if (!userActive) {
      userActive = true;
      update();
    }
    clearTimeout(afkTimer);
    afkTimer = setTimeout(() => {
      userActive = false;
      update();
    }, ms);
  };

  const onVisibility = () => {
    pageVisible = !document.hidden;
    update();
  };

  const events = [
    "mousemove",
    "mousedown",
    "keydown",
    "touchstart",
    "wheel",
    "resize",
  ];
  document.addEventListener("visibilitychange", onVisibility);
  events.forEach((e) =>
    document.addEventListener(e, onActivity, { passive: true }),
  );
  onActivity();

  return () => {
    document.removeEventListener("visibilitychange", onVisibility);
    events.forEach((e) => document.removeEventListener(e, onActivity));
    clearTimeout(afkTimer);
  };
}
