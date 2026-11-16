export const fmtTokens = (n: number) => (n >= 10_000 ? `${(n / 1000).toFixed(1)}k` : n.toLocaleString());
export const fmtTime = (ms: number) => new Date(ms).toLocaleString();
export const KINDS = ["System", "User", "Assistant", "ToolCall", "ToolResult", "File", "Memory", "Summary"];
