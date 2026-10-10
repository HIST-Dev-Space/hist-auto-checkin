// A session owns its requests and timer. Restarting invalidates all earlier work.
export function createPollingSession() {
  let active;

  function stop() {
    active?.clearTimer();
    active?.controller.abort();
    active = undefined;
  }

  function restart() {
    stop();
    const controller = new AbortController();
    let timer;
    const session = {
      controller,
      signal: controller.signal,
      isCurrent: () => active === session && !controller.signal.aborted,
      clearTimer() {
        clearTimeout(timer);
        timer = undefined;
      },
      schedule(action, delay) {
        session.clearTimer();
        if (!session.isCurrent()) return;
        timer = setTimeout(() => {
          timer = undefined;
          if (session.isCurrent()) action(session);
        }, delay);
      },
    };
    active = session;
    return session;
  }

  window.addEventListener('pagehide', stop);
  return { restart, stop };
}
