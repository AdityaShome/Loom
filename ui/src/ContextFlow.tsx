import { Icon } from "./icons";
import { fmtTokens } from "./format";
import { flowSteps, savedPercent, type Ctx, type Model } from "./graphModel";
import type { Turn } from "./api";

/** Sparkline of what the agent sent vs what the model received, per turn. */
function Spark({ turns }: { turns: Turn[] }) {
  const W = 150;
  const H = 46;
  if (turns.length < 2) return <div className="cg-spark empty">needs 2+ turns</div>;
  const max = Math.max(1, ...turns.map((t) => t.request_tokens));
  const pts = (f: (t: Turn) => number) =>
    turns.map((t, i) => `${(i / (turns.length - 1)) * W},${H - 3 - (f(t) / max) * (H - 8)}`).join(" ");
  return (
    <svg className="cg-spark" viewBox={`0 0 ${W} ${H}`} width={W} height={H} role="img" aria-label="Tokens per turn: agent sent vs model received">
      <polyline points={pts((t) => t.request_tokens)} fill="none" stroke="#60a5fa" strokeWidth={1.6} opacity={0.7} />
      <polyline points={pts((t) => t.sent_tokens)} fill="none" stroke="#34d399" strokeWidth={2} />
    </svg>
  );
}

export default function ContextFlow({ model, ctx, turns }: { model: Model; ctx: Ctx; turns: Turn[] }) {
  const steps = flowSteps(model, ctx, turns);
  const saved = savedPercent(turns);
  return (
    <section className="cg-flow" aria-label="Context flow">
      <h4>
        <Icon name="pulse" size={16} color="#60a5fa" /> Context Flow
      </h4>
      <div className="cg-steps">
        {steps.map((s, i) => (
          <div key={s.key} className="cg-stepwrap">
            <div className="cg-step" style={{ ["--c" as string]: s.color }}>
              <span className="cg-ico">
                <Icon name={s.icon} size={20} color={s.color} />
              </span>
              <span className="cg-txt">
                <b>{s.title}</b>
                <small>{s.detail}</small>
              </span>
            </div>
            {i < steps.length - 1 && <Icon name="arrow" size={16} className="cg-arrow" />}
          </div>
        ))}
        <div className="cg-saved">
          <div>
            <small>Tokens saved</small>
            <strong>{saved}%</strong>
            <small className="muted">
              {fmtTokens(turns.reduce((s, t) => s + t.request_tokens - t.sent_tokens, 0))} total
            </small>
          </div>
          <Spark turns={turns} />
        </div>
      </div>
    </section>
  );
}
