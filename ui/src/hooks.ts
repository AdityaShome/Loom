import { useCallback, useEffect, useRef, useState } from "react";
import type { LiveEvent } from "./api";

/** Run `load` now and whenever `deps` change; expose a manual reload. */
export function useLoad<T>(load: () => Promise<T>, deps: unknown[]) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const loadRef = useRef(load);
  loadRef.current = load;
  const reload = useCallback(() => {
    loadRef.current().then(
      (d) => {
        setData(d);
        setError(null);
      },
      (e) => setError(String(e.message ?? e)),
    );
  }, []);
  useEffect(reload, deps); // eslint-disable-line react-hooks/exhaustive-deps
  return { data, error, reload };
}

/** Subscribe to a session WebSocket; reconnects with backoff. Returns connection state. */
export function useLive(sessionId: string, onEvent: (e: LiveEvent) => void) {
  const [connected, setConnected] = useState(false);
  const cb = useRef(onEvent);
  cb.current = onEvent;
  useEffect(() => {
    let ws: WebSocket | null = null;
    let timer: number | undefined;
    let delay = 500;
    let stopped = false;
    const open = () => {
      const proto = location.protocol === "https:" ? "wss:" : "ws:";
      ws = new WebSocket(`${proto}//${location.host}/ws/sessions/${encodeURIComponent(sessionId)}`);
      ws.onopen = () => {
        delay = 500;
        setConnected(true);
        // Anything that happened while disconnected: resync.
        cb.current({ type: "resync", turn: 0, new: 0, missing: 0, tokens: 0 });
      };
      ws.onmessage = (m) => {
        try {
          cb.current(JSON.parse(m.data));
        } catch {
          /* ignore malformed frames */
        }
      };
      ws.onclose = () => {
        setConnected(false);
        if (!stopped) {
          timer = window.setTimeout(open, delay);
          delay = Math.min(delay * 2, 8000);
        }
      };
    };
    open();
    return () => {
      stopped = true;
      window.clearTimeout(timer);
      ws?.close();
    };
  }, [sessionId]);
  return connected;
}

export function useInterval(fn: () => void, ms: number) {
  const ref = useRef(fn);
  ref.current = fn;
  useEffect(() => {
    const t = window.setInterval(() => ref.current(), ms);
    return () => window.clearInterval(t);
  }, [ms]);
}

/** Current location.hash path, e.g. "/s/abc". */
export function useRoute() {
  const [route, setRoute] = useState(() => location.hash.slice(1) || "/");
  useEffect(() => {
    const on = () => setRoute(location.hash.slice(1) || "/");
    window.addEventListener("hashchange", on);
    return () => window.removeEventListener("hashchange", on);
  }, []);
  return route;
}
