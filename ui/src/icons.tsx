import type { ReactNode } from "react";
import type { IconName } from "./graphModel";

type Props = { size?: number; color?: string; className?: string };

function svg(children: ReactNode, { size = 20, color = "currentColor", className }: Props) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke={color}
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

export function Icon({ name, ...p }: Props & { name: IconName | "check" | "alert" | "search" | "filter" | "fit" | "expand" | "bulb" | "link" | "arrow" | "layers" | "clock" | "graph" }) {
  switch (name) {
    case "shield":
      return svg(<path d="M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z" />, p);
    case "user":
      return svg(
        <>
          <circle cx="12" cy="8" r="4" />
          <path d="M4 21c0-4 3.6-6 8-6s8 2 8 6" />
        </>,
        p,
      );
    case "wrench":
      return svg(<path d="M14.7 6.3a4 4 0 0 0-5.4 5.1L3 17.7 6.3 21l6.3-6.3a4 4 0 0 0 5.1-5.4l-2.6 2.6-2.4-.6-.6-2.4z" />, p);
    case "chat":
      return svg(<path d="M21 12a8 8 0 0 1-11.8 7L4 20l1-4.2A8 8 0 1 1 21 12z" />, p);
    case "book":
      return svg(
        <>
          <path d="M3 5.5C5 4 8 4 12 6c4-2 7-2 9-.5V19c-2-1.5-5-1.5-9 .5-4-2-7-2-9-.5z" />
          <path d="M12 6v13.5" />
        </>,
        p,
      );
    case "pulse":
      return svg(<polyline points="3 12 7 12 10 5 14 19 17 12 21 12" />, p);
    case "cpu":
      return svg(
        <>
          <rect x="6" y="6" width="12" height="12" rx="2.5" />
          <rect x="9.5" y="9.5" width="5" height="5" rx="1" />
          <path d="M9 3v3M15 3v3M9 18v3M15 18v3M3 9h3M3 15h3M18 9h3M18 15h3" />
        </>,
        p,
      );
    case "sliders":
      return svg(
        <>
          <path d="M4 7h9M17 7h3M4 17h3M11 17h9" />
          <circle cx="15" cy="7" r="2" />
          <circle cx="9" cy="17" r="2" />
        </>,
        p,
      );
    case "gauge":
      return svg(
        <>
          <path d="M4.5 17a8 8 0 1 1 15 0" />
          <path d="M12 16l3.5-5" />
          <circle cx="12" cy="16" r="1.3" />
        </>,
        p,
      );
    case "bolt":
      return svg(<path d="M13 3L5 14h6l-1 7 8-11h-6z" />, p);
    case "copy":
      return svg(
        <>
          <rect x="8" y="8" width="12" height="12" rx="2" />
          <path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2" />
        </>,
        p,
      );
    case "alert":
      return svg(
        <>
          <circle cx="12" cy="12" r="9" />
          <path d="M12 7.5v5M12 16h.01" />
        </>,
        p,
      );
    case "check":
      return svg(<path d="M5 12.5l4.2 4.2L19 7" />, p);
    case "search":
      return svg(
        <>
          <circle cx="11" cy="11" r="6.5" />
          <path d="M20 20l-4.2-4.2" />
        </>,
        p,
      );
    case "filter":
      return svg(<path d="M3 5h18l-7 8.5V20l-4-2v-4.5z" />, p);
    case "fit":
      return svg(<path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" />, p);
    case "expand":
      return svg(<path d="M9 4H4v5M15 4h5v5M9 20H4v-5M15 20h5v-5M4 4l6 6M20 4l-6 6M4 20l6-6M20 20l-6-6" />, p);
    case "bulb":
      return svg(
        <>
          <path d="M9 18h6M10 21h4" />
          <path d="M12 3a6 6 0 0 0-3.5 10.9c.6.5 1 1.2 1 2.1h5c0-.9.4-1.6 1-2.1A6 6 0 0 0 12 3z" />
        </>,
        p,
      );
    case "link":
      return svg(
        <>
          <path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1" />
          <path d="M14 10a4 4 0 0 0-5.7 0l-3 3A4 4 0 0 0 11 18.7l1-1" />
        </>,
        p,
      );
    case "arrow":
      return svg(<path d="M5 12h14M13 6l6 6-6 6" />, p);
    case "layers":
      return svg(
        <>
          <path d="M12 3l9 5-9 5-9-5z" />
          <path d="M3 13l9 5 9-5" />
        </>,
        p,
      );
    case "clock":
      return svg(
        <>
          <circle cx="12" cy="12" r="8.5" />
          <path d="M12 7v5l3 2" />
        </>,
        p,
      );
    case "graph":
      return svg(
        <>
          <circle cx="6" cy="6" r="2.2" />
          <circle cx="18" cy="8" r="2.2" />
          <circle cx="12" cy="18" r="2.2" />
          <path d="M8 6.4l8 1.2M7.2 8l3.6 8M16.8 10l-3.6 6" />
        </>,
        p,
      );
  }
}
