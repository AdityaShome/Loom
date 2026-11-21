import { useEffect, useState } from "react";

/** A button that needs a second click within 3 seconds, so a delete cannot happen by accident. */
export default function ConfirmButton({
  label,
  confirmLabel = "Really delete?",
  onConfirm,
  className = "",
  title,
}: {
  label: string;
  confirmLabel?: string;
  onConfirm: () => void;
  className?: string;
  title?: string;
}) {
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const t = window.setTimeout(() => setArmed(false), 3000);
    return () => window.clearTimeout(t);
  }, [armed]);
  return (
    <button
      type="button"
      className={`${className} ${armed ? "armed" : ""}`}
      title={title}
      onClick={() => {
        if (armed) {
          setArmed(false);
          onConfirm();
        } else setArmed(true);
      }}
    >
      {armed ? confirmLabel : label}
    </button>
  );
}
