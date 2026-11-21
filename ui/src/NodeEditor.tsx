import { useEffect, useRef, useState } from "react";
import { api, type NodeDetail } from "./api";
import { estimateTokens } from "./knowledgeModel";

const NOT_EDITABLE = ["System", "ToolCall", "Summary"];

/** Edit what the model sees for one context node. The original is never lost; reverting puts it back. */
export default function NodeEditor({
  nodeId,
  mode,
  onClose,
  onChanged,
}: {
  nodeId: string;
  mode: string;
  onClose: () => void;
  onChanged: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [detail, setDetail] = useState<NodeDetail | null>(null);
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api.nodeDetail(nodeId).then(
      (d) => {
        setDetail(d);
        setText(d.override ?? d.content);
      },
      (e) => setError(String(e.message ?? e)),
    );
  }, [nodeId]);

  useEffect(() => {
    const d = dialog.current;
    if (d && !d.open) d.showModal();
    return () => d?.close();
  }, []);

  const run = (p: Promise<void>) => {
    setBusy(true);
    p.then(
      () => {
        onChanged();
        onClose();
      },
      (e) => {
        setError(String(e.message ?? e));
        setBusy(false);
      },
    );
  };

  const editable = detail && !detail.locked && !NOT_EDITABLE.includes(detail.kind);
  const current = detail?.override ?? detail?.content ?? "";
  const unchanged = text === current || text.trim() === "";

  return (
    <dialog ref={dialog} className="nodeeditor" onClose={onClose} aria-label="Edit node text">
      <header>
        <h3>Edit what the model sees</h3>
        {detail && (
          <p className="muted">
            {detail.kind}
            {detail.source ? ` · ${detail.source}` : ""} · {detail.tokens.toLocaleString()} tokens
          </p>
        )}
      </header>

      {error && <p className="error">{error}</p>}
      {!detail && !error && <p className="muted">Loading…</p>}

      {detail && !editable && (
        <p className="banner">
          {detail.kind === "System" || detail.kind === "ToolCall"
            ? `${detail.kind} nodes cannot be edited: their exact form matters to the provider. Use Drop to remove one.`
            : "This block cannot be edited (the provider needs it back exactly as it was)."}
        </p>
      )}

      {detail && editable && (
        <>
          {mode !== "Enforce" && (
            <p className="banner">
              Saved, but not sent yet: this session is in <b>{mode}</b> mode. Edits reach the model when you switch to <b>Enforce</b> (Plan tab).
            </p>
          )}
          <details open={!detail.override}>
            <summary>Original text</summary>
            <pre className="orig">{detail.content}</pre>
          </details>
          <label className="field">
            Your version
            <textarea value={text} onChange={(e) => setText(e.target.value)} rows={10} spellCheck={false} autoFocus />
          </label>
          <p className="muted small">
            About {estimateTokens(text).toLocaleString()} tokens (original {estimateTokens(detail.content).toLocaleString()}). The model sees your text in place of the
            original; the tool call or message around it stays as it is.
          </p>
        </>
      )}

      <footer>
        {detail && editable && (
          <>
            <button className="primary" disabled={busy || unchanged} onClick={() => run(api.setNodeText(nodeId, text))}>
              Save
            </button>
            {detail.override != null && (
              <button disabled={busy} onClick={() => run(api.clearNodeText(nodeId))}>
                Use the original again
              </button>
            )}
          </>
        )}
        <button disabled={busy} onClick={onClose}>
          {detail && editable ? "Cancel" : "Close"}
        </button>
      </footer>
    </dialog>
  );
}
