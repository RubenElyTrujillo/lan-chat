import type { History } from "../types";

const KEY = "lanchat.history.v1";

export function loadHistory(): History {
  try {
    const raw = localStorage.getItem(KEY);
    return raw ? (JSON.parse(raw) as History) : {};
  } catch {
    return {};
  }
}

export function saveHistory(history: History): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(history));
  } catch {
    return;
  }
}
