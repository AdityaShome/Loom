import { useRoute } from "./hooks";
import Sessions from "./Sessions";
import SessionView from "./SessionView";

export default function App() {
  const route = useRoute();
  const m = route.match(/^\/s\/(.+)$/);
  return (
    <div className="app">
      <header className="top">
        <a href="#/" className="brand">
          Loom
        </a>
        <span className="muted">agent context control plane</span>
      </header>
      <main>{m ? <SessionView id={decodeURIComponent(m[1])} /> : <Sessions />}</main>
    </div>
  );
}
