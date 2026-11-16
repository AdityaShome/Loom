import { useMemo, useState } from "react";
import {
  createColumnHelper,
  flexRender,
  getCoreRowModel,
  getSortedRowModel,
  useReactTable,
  type SortingState,
} from "@tanstack/react-table";
import type { NodeRow } from "./api";
import { fmtTokens, KINDS } from "./format";

const col = createColumnHelper<NodeRow>();
const isNum = (def: unknown) => (def as { meta?: { num?: boolean } }).meta?.num ?? false;

export default function ContextList({ nodes, latest }: { nodes: NodeRow[]; latest: number }) {
  const [kind, setKind] = useState("");
  const [q, setQ] = useState("");
  const [showGone, setShowGone] = useState(false);
  const [sorting, setSorting] = useState<SortingState>([]);

  const rows = useMemo(() => {
    const needle = q.trim().toLowerCase();
    return nodes.filter(
      (n) =>
        (showGone || n.status !== "DroppedByAgent") &&
        (!kind || n.kind === kind) &&
        (!needle || n.preview.toLowerCase().includes(needle) || (n.source ?? "").toLowerCase().includes(needle)),
    );
  }, [nodes, kind, q, showGone]);

  const columns = useMemo(
    () => [
      col.accessor("position", { header: "#", cell: (c) => c.getValue() + 1 }),
      col.accessor("kind", {
        header: "Kind",
        cell: (c) => <span className={`badge k-${c.getValue()}`}>{c.getValue()}</span>,
      }),
      col.display({
        id: "content",
        header: "Content",
        enableSorting: false,
        cell: ({ row: { original: n } }) => (
          <div className="content">
            {n.source && <code className="src">{n.source}</code>}
            <span className="preview">{n.preview || <em className="muted">(empty)</em>}</span>
          </div>
        ),
      }),
      col.accessor("tokens", { header: "Tokens", cell: (c) => fmtTokens(c.getValue()), meta: { num: true } }),
      col.accessor("first_seen_turn", {
        header: "Seen",
        cell: (c) => `t${c.getValue() + 1}–t${c.row.original.last_seen_turn + 1}`,
      }),
      col.accessor("status", {
        header: "Status",
        cell: (c) => (
          <span className={`status s-${c.getValue()}`}>
            {c.getValue() === "DroppedByAgent" ? "dropped by agent" : c.getValue().toLowerCase()}
          </span>
        ),
      }),
    ],
    [],
  );

  const table = useReactTable({
    data: rows,
    columns,
    state: { sorting },
    onSortingChange: setSorting,
    getCoreRowModel: getCoreRowModel(),
    getSortedRowModel: getSortedRowModel(),
    getRowId: (n) => n.id,
  });

  return (
    <>
      <div className="filters">
        <select value={kind} onChange={(e) => setKind(e.target.value)} aria-label="Filter by kind">
          <option value="">All kinds</option>
          {KINDS.map((k) => (
            <option key={k}>{k}</option>
          ))}
        </select>
        <input value={q} onChange={(e) => setQ(e.target.value)} placeholder="Search content or source…" />
        <label>
          <input type="checkbox" checked={showGone} onChange={(e) => setShowGone(e.target.checked)} /> show nodes the
          agent dropped
        </label>
        <span className="muted">{rows.length} shown</span>
      </div>
      <table className="grid">
        <thead>
          {table.getHeaderGroups().map((hg) => (
            <tr key={hg.id}>
              {hg.headers.map((h) => (
                <th
                  key={h.id}
                  className={isNum(h.column.columnDef) ? "num" : ""}
                  onClick={h.column.getToggleSortingHandler()}
                  style={{ cursor: h.column.getCanSort() ? "pointer" : "default" }}
                >
                  {flexRender(h.column.columnDef.header, h.getContext())}
                  {{ asc: " ▲", desc: " ▼" }[h.column.getIsSorted() as string] ?? ""}
                </th>
              ))}
            </tr>
          ))}
        </thead>
        <tbody>
          {table.getRowModel().rows.map((r) => (
            <tr
              key={r.id}
              className={r.original.last_seen_turn === latest && r.original.status !== "DroppedByAgent" ? "" : "gone"}
            >
              {r.getVisibleCells().map((c) => (
                <td key={c.id} className={isNum(c.column.columnDef) ? "num" : ""}>
                  {flexRender(c.column.columnDef.cell, c.getContext())}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </>
  );
}
