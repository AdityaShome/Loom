import { useEffect, useMemo, useRef, useState } from "react";
import cytoscape from "cytoscape";
import { api, type Entity, type EntityKind, type Fact, type Knowledge, type NodeRow } from "./api";
import { useLoad } from "./hooks";
import { Icon } from "./icons";
import ConfirmButton from "./ConfirmButton";
import {
  buildElements,
  byId,
  counts,
  factsAbout,
  factSummary,
  groupByKind,
  KIND_META,
  KINDS,
  NO_FILTER,
  PREDICATES,
  shortName,
  SOURCE_LABEL,
  visibleEntities,
  type Filter,
  type GraphEl,
} from "./knowledgeModel";
import "./contextgraph.css";
import "./knowledge.css";

type Sel = { type: "entity" | "fact"; id: string } | null;
type Act = (p: Promise<unknown>) => void;

const STYLE: cytoscape.StylesheetJson = [
  {
    selector: "node",
    style: {
      shape: "data(shape)" as never,
      "background-color": "data(color)",
      "background-opacity": 0.22,
      "border-width": 2,
      "border-color": "data(color)",
      width: "data(size)",
      height: "data(size)",
      label: "data(label)",
      color: "#e6ebf5",
      "font-size": 10,
      "text-valign": "bottom",
      "text-margin-y": 4,
      "text-wrap": "ellipsis",
      "text-max-width": "130px",
    },
  },
  { selector: "node[source = 'user']", style: { "border-style": "double", "border-width": 4 } },
  { selector: "node[?sent]", style: { "border-color": "#facc15", "border-width": 4 } },
  {
    selector: "edge",
    style: {
      width: 1.6,
      "line-color": "#475569",
      "target-arrow-color": "#475569",
      "target-arrow-shape": "triangle",
      "arrow-scale": 0.8,
      "curve-style": "bezier",
      label: "data(label)",
      "font-size": 9,
      color: "#94a3b8",
      "text-rotation": "autorotate",
      "text-background-color": "#0a0e19",
      "text-background-opacity": 1,
      "text-background-padding": "2px",
    },
  },
  { selector: "edge[origin = 'user']", style: { "line-style": "dashed" } },
  { selector: "edge[?sent]", style: { "line-color": "#facc15", "target-arrow-color": "#facc15", width: 3 } },
  { selector: "node:selected", style: { "border-color": "#ffffff", "border-width": 5, "overlay-opacity": 0.12 } },
  { selector: "edge:selected", style: { "line-color": "#ffffff", "target-arrow-color": "#ffffff", width: 3.5 } },
];

/** Fit everything in view, but never zoom in past 1.1x: a handful of nodes should not fill the screen. */
function fitView(c: cytoscape.Core) {
  c.fit(undefined, 40);
  if (c.zoom() > 1.1) {
    c.zoom(1.1);
    c.center();
  }
}

function Graph({ els, selection, onSelect, fitSignal }: { els: GraphEl[]; selection: Sel; onSelect: (s: Sel) => void; fitSignal: number }) {
  const box = useRef<HTMLDivElement>(null);
  const cy = useRef<cytoscape.Core | null>(null);
  const known = useRef("");
  const pick = useRef(onSelect);
  pick.current = onSelect;

  useEffect(() => {
    const c = cytoscape({ container: box.current, minZoom: 0.2, maxZoom: 3, wheelSensitivity: 0.25, style: STYLE });
    cy.current = c;
    c.on("tap", "node", (e) => pick.current({ type: "entity", id: e.target.id() }));
    c.on("tap", "edge", (e) => pick.current({ type: "fact", id: e.target.id() }));
    c.on("tap", (e) => e.target === c && pick.current(null));
    return () => {
      c.destroy();
      cy.current = null;
    };
  }, []);

  // New or removed things re-run the layout; plain edits update in place so the picture does not jump.
  useEffect(() => {
    const c = cy.current;
    if (!c) return;
    const key = els.map((e) => e.data.id).sort().join("|");
    const rebuilt = key !== known.current;
    c.batch(() => {
      if (rebuilt) {
        c.elements().remove();
        c.add(els as cytoscape.ElementDefinition[]);
      } else {
        els.forEach((e) => c.getElementById(e.data.id).data(e.data));
      }
    });
    if (rebuilt) {
      known.current = key;
      if (els.length) {
        c.layout({ name: "cose", animate: false, padding: 40, nodeRepulsion: () => 26000, idealEdgeLength: () => 120, componentSpacing: 110, nodeOverlap: 40 } as cytoscape.LayoutOptions).run();
        fitView(c);
      }
    }
  }, [els]);

  useEffect(() => {
    const c = cy.current;
    if (!c) return;
    c.elements().unselect();
    if (selection) c.getElementById(selection.id).select();
  }, [selection, els]);

  useEffect(() => {
    if (fitSignal && cy.current) fitView(cy.current);
  }, [fitSignal]);

  return <div ref={box} className="kg-canvas" role="img" aria-label="Knowledge graph" />;
}

function SourceBadge({ source }: { source: string }) {
  return <span className={`src-badge s-${source}`}>{SOURCE_LABEL[source as keyof typeof SOURCE_LABEL] ?? source}</span>;
}

/** One fact, with the controls to send it to the model, change it, or delete it. */
function FactRow({
  fact,
  k,
  act,
  nodes,
  onNode,
  startOpen = false,
}: {
  fact: Fact;
  k: Knowledge;
  act: Act;
  nodes: Map<string, NodeRow>;
  onNode: (id: string) => void;
  startOpen?: boolean;
}) {
  const ents = useMemo(() => byId(k), [k]);
  const [open, setOpen] = useState(startOpen);
  const [predicate, setPredicate] = useState(fact.predicate);
  const [text, setText] = useState(fact.text);
  const [object, setObject] = useState(fact.object_id ?? "");
  useEffect(() => {
    setPredicate(fact.predicate);
    setText(fact.text);
    setObject(fact.object_id ?? "");
  }, [fact]);
  const dirty = predicate.trim() !== fact.predicate || text.trim() !== fact.text || (object || null) !== fact.object_id;
  const node = fact.node_id ? nodes.get(fact.node_id) : undefined;

  return (
    <li className={`factrow ${fact.inject ? "sent" : ""}`} data-fact={fact.id}>
      <div className="factline">
        <label className="sendtoggle" title="Send this fact to the model (Enforce mode)">
          <input type="checkbox" checked={fact.inject} onChange={(e) => act(api.updateFact(fact.id, { inject: e.target.checked }))} aria-label="Send to the model" />
          <Icon name="bolt" size={14} />
        </label>
        <span className="summary">{factSummary(fact, ents)}</span>
        <SourceBadge source={fact.source} />
      </div>
      <div className="factactions">
        <button type="button" onClick={() => setOpen(!open)}>
          {open ? "Close" : "Edit"}
        </button>
        {node && (
          <button type="button" title={`From a ${node.kind} node: open it to edit its text`} onClick={() => onNode(node.id)}>
            Source text
          </button>
        )}
        <ConfirmButton label="Delete" onConfirm={() => act(api.deleteFact(fact.id))} className="danger" />
      </div>
      {open && (
        <form
          className="factform"
          onSubmit={(e) => {
            e.preventDefault();
            act(api.updateFact(fact.id, { predicate: predicate.trim(), text: text.trim(), object_id: object || null }));
          }}
        >
          <label>
            Relation
            <input value={predicate} onChange={(e) => setPredicate(e.target.value)} list="kg-predicates" required />
          </label>
          <label>
            To
            <select value={object} onChange={(e) => setObject(e.target.value)}>
              <option value="">(nothing: a note about the subject)</option>
              {k.entities
                .filter((x) => x.id !== fact.subject_id)
                .map((x) => (
                  <option key={x.id} value={x.id}>
                    {x.kind}: {shortName(x.name, 40)}
                  </option>
                ))}
            </select>
          </label>
          <label>
            Details
            <textarea value={text} onChange={(e) => setText(e.target.value)} rows={2} />
          </label>
          <button type="submit" className="primary" disabled={!dirty || !predicate.trim()}>
            Save fact
          </button>
        </form>
      )}
    </li>
  );
}

function AddFact({ entity, k, act }: { entity: Entity; k: Knowledge; act: Act }) {
  const [predicate, setPredicate] = useState("");
  const [object, setObject] = useState("");
  const [text, setText] = useState("");
  const [inject, setInject] = useState(true);
  return (
    <form
      className="addfact"
      onSubmit={(e) => {
        e.preventDefault();
        act(api.createFact(entity.session_id, { subject_id: entity.id, predicate: predicate.trim(), object_id: object || null, text: text.trim(), inject }));
        setPredicate("");
        setObject("");
        setText("");
      }}
    >
      <h5>Add a fact about this</h5>
      <div className="row">
        <input value={predicate} onChange={(e) => setPredicate(e.target.value)} placeholder="relation, e.g. has_bug" list="kg-predicates" aria-label="Relation" required />
        <select value={object} onChange={(e) => setObject(e.target.value)} aria-label="Linked to">
          <option value="">no link (a note)</option>
          {k.entities
            .filter((x) => x.id !== entity.id)
            .map((x) => (
              <option key={x.id} value={x.id}>
                {x.kind}: {shortName(x.name, 40)}
              </option>
            ))}
        </select>
      </div>
      <textarea value={text} onChange={(e) => setText(e.target.value)} placeholder="what should be remembered?" rows={2} aria-label="Details" />
      <div className="row">
        <label className="sendtoggle">
          <input type="checkbox" checked={inject} onChange={(e) => setInject(e.target.checked)} /> <Icon name="bolt" size={14} /> send to the model
        </label>
        <button type="submit" className="primary" disabled={!predicate.trim()}>
          Add fact
        </button>
      </div>
    </form>
  );
}

function EntityEditor({ entity, k, act, nodes, onNode, onDeleted }: { entity: Entity; k: Knowledge; act: Act; nodes: Map<string, NodeRow>; onNode: (id: string) => void; onDeleted: () => void }) {
  const [name, setName] = useState(entity.name);
  const [kind, setKind] = useState<EntityKind>(entity.kind);
  const [note, setNote] = useState(entity.note ?? "");
  useEffect(() => {
    setName(entity.name);
    setKind(entity.kind);
    setNote(entity.note ?? "");
  }, [entity]);
  const facts = factsAbout(k, entity.id);
  const dirty = name.trim() !== entity.name || kind !== entity.kind || note.trim() !== (entity.note ?? "");

  return (
    <div className="kg-editor">
      <header className="kg-eh">
        <span className="kinddot" style={{ background: KIND_META[kind].color }} />
        <h3>{entity.name}</h3>
        <SourceBadge source={entity.source} />
      </header>
      <p className="muted small">{KIND_META[entity.kind].blurb}</p>
      <form
        className="entityform"
        onSubmit={(e) => {
          e.preventDefault();
          act(api.updateEntity(entity.id, { name: name.trim(), kind, note: note.trim() || null }));
        }}
      >
        <label>
          Name
          <input value={name} onChange={(e) => setName(e.target.value)} required />
        </label>
        <label>
          Kind
          <select value={kind} onChange={(e) => setKind(e.target.value as EntityKind)}>
            {KINDS.map((x) => (
              <option key={x}>{x}</option>
            ))}
          </select>
        </label>
        <label>
          Note
          <textarea value={note} onChange={(e) => setNote(e.target.value)} rows={2} placeholder="anything you want to remember about it" />
        </label>
        <div className="row">
          <button type="submit" className="primary" disabled={!dirty || !name.trim()}>
            Save
          </button>
          <ConfirmButton
            label="Delete entity"
            confirmLabel="Delete it and its facts?"
            className="danger"
            onConfirm={() => {
              act(api.deleteEntity(entity.id));
              onDeleted();
            }}
          />
        </div>
      </form>

      <h4>
        Facts <span className="count">{facts.length}</span>
      </h4>
      {facts.length === 0 ? (
        <p className="muted small">No facts yet.</p>
      ) : (
        <ul className="facts">
          {facts.map((f) => (
            <FactRow key={f.id} fact={f} k={k} act={act} nodes={nodes} onNode={onNode} />
          ))}
        </ul>
      )}
      <AddFact entity={entity} k={k} act={act} />
    </div>
  );
}

function AddEntity({ session, act, onCreated }: { session: string; act: Act; onCreated: () => void }) {
  const [kind, setKind] = useState<EntityKind>("Concept");
  const [name, setName] = useState("");
  const [note, setNote] = useState("");
  return (
    <form
      className="kg-editor addentity"
      onSubmit={(e) => {
        e.preventDefault();
        act(api.createEntity(session, { kind, name: name.trim(), note: note.trim() || undefined }));
        setName("");
        setNote("");
        onCreated();
      }}
    >
      <h3>Add something</h3>
      <p className="muted small">A concept, a rule, a person's preference, a file you want remembered. Then link it to other things with facts.</p>
      <label>
        Kind
        <select value={kind} onChange={(e) => setKind(e.target.value as EntityKind)} aria-label="Kind">
          {KINDS.map((x) => (
            <option key={x}>{x}</option>
          ))}
        </select>
      </label>
      <label>
        Name
        <input value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. rounding rules" aria-label="Name" required autoFocus />
      </label>
      <label>
        Note
        <textarea value={note} onChange={(e) => setNote(e.target.value)} rows={3} aria-label="Note" />
      </label>
      <button type="submit" className="primary" disabled={!name.trim()}>
        Add
      </button>
    </form>
  );
}

export default function KnowledgeView({
  sessionId,
  mode,
  tick,
  onEditText,
}: {
  sessionId: string;
  mode: string;
  tick: number;
  onEditText: (nodeId: string) => void;
}) {
  const kg = useLoad(() => api.knowledge(sessionId), [sessionId, tick]);
  const nodes = useLoad(() => api.nodes(sessionId), [sessionId, tick]);
  const [sel, setSel] = useState<Sel>(null);
  const [adding, setAdding] = useState(false);
  const [filter, setFilter] = useState<Filter>(NO_FILTER);
  const [fit, setFit] = useState(0);
  const [error, setError] = useState<string | null>(null);

  const k = kg.data;
  const nodeMap = useMemo(() => new Map((nodes.data ?? []).map((n) => [n.id, n])), [nodes.data]);
  const els = useMemo(() => (k ? buildElements(k, filter) : []), [k, filter]);
  const visible = useMemo(() => (k ? visibleEntities(k, filter) : []), [k, filter]);
  const ents = useMemo(() => (k ? byId(k) : new Map<string, Entity>()), [k]);

  // A selection that was deleted (here or elsewhere) goes away.
  useEffect(() => {
    if (!k || !sel) return;
    const gone = sel.type === "entity" ? !ents.has(sel.id) : !k.facts.some((f) => f.id === sel.id);
    if (gone) setSel(null);
  }, [k, sel, ents]);

  const act: Act = (p) =>
    p.then(
      () => {
        setError(null);
        kg.reload();
      },
      (e) => setError(String(e.message ?? e)),
    );

  if (kg.error) return <p className="error">{kg.error}</p>;
  if (!k) return <p className="muted">Loading…</p>;
  const c = counts(k);
  const entity = sel?.type === "entity" ? ents.get(sel.id) : undefined;
  const fact = sel?.type === "fact" ? k.facts.find((f) => f.id === sel.id) : undefined;
  const select = (s: Sel) => {
    setSel(s);
    setAdding(false);
  };
  const toggleKind = (kind: EntityKind) =>
    setFilter((f) => {
      const hidden = new Set(f.hiddenKinds);
      if (!hidden.delete(kind)) hidden.add(kind);
      return { ...f, hiddenKinds: hidden };
    });
  const editedNodes = k.edited_nodes.map((id) => nodeMap.get(id)).filter((n): n is NodeRow => !!n);

  return (
    <section className="cg kg" aria-label="Knowledge graph">
      <datalist id="kg-predicates">
        {PREDICATES.map((p) => (
          <option key={p} value={p} />
        ))}
      </datalist>
      <header className="cg-head">
        <div>
          <h3>Knowledge Graph</h3>
          <p>What the agent has seen and done, as things and facts about them. Edit anything; mark facts to send them to the model.</p>
        </div>
        <div className="kg-stats">
          <span>
            <b>{c.entities}</b> things
          </span>
          <span>
            <b>{c.facts}</b> facts
          </span>
          <span className={c.sent ? "sent" : ""}>
            <b>{c.sent}</b> sent
          </span>
          <span>
            <b>{c.mine}</b> yours
          </span>
        </div>
      </header>

      <div className="kg-toolbar">
        <label className="cg-search">
          <Icon name="search" size={16} />
          <input value={filter.query} onChange={(e) => setFilter({ ...filter, query: e.target.value })} placeholder="Search things and facts…" aria-label="Search the knowledge graph" />
        </label>
        <div className="kg-kinds" role="group" aria-label="Show kinds">
          {KINDS.filter((kind) => k.entities.some((e) => e.kind === kind)).map((kind) => (
            <button
              key={kind}
              type="button"
              className={`chip ${filter.hiddenKinds.has(kind) ? "off" : ""}`}
              aria-pressed={!filter.hiddenKinds.has(kind)}
              onClick={() => toggleKind(kind)}
              style={{ ["--c" as string]: KIND_META[kind].color }}
            >
              <i className="cg-dot" /> {kind}
            </button>
          ))}
        </div>
        <label className="onlysent">
          <input type="checkbox" checked={filter.onlySent} onChange={(e) => setFilter({ ...filter, onlySent: e.target.checked })} /> only what is sent
        </label>
        <button type="button" className="cg-iconbtn" onClick={() => setFit((n) => n + 1)} aria-label="Fit to view" title="Fit to view">
          <Icon name="fit" size={17} />
        </button>
        <button
          type="button"
          className="primary"
          onClick={() => {
            setAdding(true);
            setSel(null);
          }}
        >
          + Add
        </button>
      </div>

      {error && <p className="error kg-error">{error}</p>}

      <div className="kg-body">
        <nav className="kg-list" aria-label="Things">
          {visible.length === 0 && <p className="muted small pad">Nothing matches.</p>}
          {groupByKind(visible).map(([kind, list]) => (
            <div key={kind}>
              <h5 style={{ color: KIND_META[kind].color }}>
                {kind} <span className="count">{list.length}</span>
              </h5>
              {list.map((e) => {
                const mine = k.facts.filter((f) => f.subject_id === e.id || f.object_id === e.id);
                return (
                  <button key={e.id} type="button" className={`kg-item ${sel?.type === "entity" && sel.id === e.id ? "active" : ""}`} onClick={() => select({ type: "entity", id: e.id })} title={e.name}>
                    <i className="cg-dot" style={{ background: KIND_META[kind].color }} />
                    <span className="name">{shortName(e.name, 30)}</span>
                    {mine.some((f) => f.inject) && <Icon name="bolt" size={13} color="#facc15" />}
                    {e.source === "user" && <span className="you">you</span>}
                    <span className="n">{mine.length}</span>
                  </button>
                );
              })}
            </div>
          ))}
        </nav>

        <div className="kg-graph">
          {els.length === 0 ? <div className="cg-empty">{k.entities.length ? "Everything is filtered out." : "Nothing extracted yet. Entities appear as the agent reads files and runs commands."}</div> : null}
          <Graph els={els} selection={sel} onSelect={select} fitSignal={fit} />
          <p className="kg-legend">
            <span>
              <i className="swatch sent" /> sent to the model
            </span>
            <span>
              <i className="swatch you" /> added by you
            </span>
            <span>dashed line: your own link</span>
          </p>
        </div>

        <aside className="kg-panel" aria-label="Editor">
          {adding ? (
            <AddEntity session={sessionId} act={act} onCreated={() => setAdding(false)} />
          ) : entity ? (
            <EntityEditor entity={entity} k={k} act={act} nodes={nodeMap} onNode={onEditText} onDeleted={() => setSel(null)} />
          ) : fact ? (
            <div className="kg-editor">
              <h3>Fact</h3>
              <ul className="facts">
                <FactRow fact={fact} k={k} act={act} nodes={nodeMap} onNode={onEditText} startOpen />
              </ul>
              <button type="button" onClick={() => setSel({ type: "entity", id: fact.subject_id })}>
                Go to {shortName(ents.get(fact.subject_id)?.name ?? "subject", 24)}
              </button>
            </div>
          ) : (
            <div className="kg-editor">
              <h3>Select something</h3>
              <p className="muted small">Click a thing in the list or the graph to see and change it. Lines are facts that link two things; click a line to edit it.</p>
              <ul className="kg-tips">
                <li>
                  <Icon name="bolt" size={14} color="#facc15" /> Tick the bolt on a fact to send it to the model.
                </li>
                <li>Delete something extracted and it stays deleted.</li>
                <li>Change what a message or tool result says from the Context tab (Edit text).</li>
              </ul>
            </div>
          )}
        </aside>
      </div>

      <section className="kg-preview" aria-label="What the model will be told">
        <header>
          <h4>
            <Icon name="bolt" size={16} color="#facc15" /> What the model will be told
          </h4>
          {k.memory && <span className="muted small">{k.memory.facts} facts · about {k.memory.tokens.toLocaleString()} tokens, taken from the budget</span>}
        </header>
        {mode !== "Enforce" && (
          <p className="banner">
            Not sent yet: this session is in <b>{mode}</b> mode. Notes and edited text only reach the model in <b>Enforce</b> mode (Plan tab).
          </p>
        )}
        {k.memory ? <pre className="memory">{k.memory.text}</pre> : <p className="muted small">No facts are marked to send. Tick the bolt on a fact to include it. It is appended to the end of the last user message.</p>}
        {editedNodes.length > 0 && (
          <div className="edited">
            <b>{editedNodes.length}</b> context {editedNodes.length === 1 ? "node has" : "nodes have"} your text instead of the original:
            {editedNodes.map((n) => (
              <button key={n.id} type="button" onClick={() => onEditText(n.id)} title="Edit again or restore the original">
                {n.kind}
                {n.source ? `: ${shortName(n.source, 24)}` : ""}
              </button>
            ))}
          </div>
        )}
      </section>
    </section>
  );
}
