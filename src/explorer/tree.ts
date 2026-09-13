import type { Node, NodeKind } from "../types";

export type ExplorerLanguage = "zh" | "en";

export function childrenByParent(nodes: Node[]): Map<string | null, Node[]> {
  const map = new Map<string | null, Node[]>();
  for (const node of nodes) {
    const list = map.get(node.parentId);
    if (list) list.push(node);
    else map.set(node.parentId, [node]);
  }
  return map;
}

// Folders first, then sessions and materials interleaved (VS Code order).
const kindRank: Record<NodeKind, number> = { folder: 0, session: 1, material: 1 };

export function sortChildren(nodes: Node[], language: ExplorerLanguage): Node[] {
  const collator = new Intl.Collator(language === "zh" ? "zh" : "en", {
    numeric: true,
    sensitivity: "base",
  });
  return [...nodes].sort((a, b) => {
    const rankDiff = kindRank[a.kind] - kindRank[b.kind];
    if (rankDiff !== 0) return rankDiff;
    const nameDiff = collator.compare(a.name, b.name);
    if (nameDiff !== 0) return nameDiff;
    // Stable tiebreak for duplicate names, since siblings need not be unique.
    return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
  });
}

export interface FlatRow {
  node: Node;
  depth: number;
  posInSet: number;
  setSize: number;
}

export function flattenVisible(
  nodes: Node[],
  expanded: Set<string>,
  language: ExplorerLanguage,
): FlatRow[] {
  const byParent = childrenByParent(nodes);
  const rows: FlatRow[] = [];
  const walk = (parentId: string | null, depth: number) => {
    const children = sortChildren(byParent.get(parentId) ?? [], language);
    children.forEach((node, index) => {
      rows.push({ node, depth, posInSet: index + 1, setSize: children.length });
      if (node.kind === "folder" && expanded.has(node.id)) walk(node.id, depth + 1);
    });
  };
  walk(null, 0);
  return rows;
}

export function ancestorsOf(nodes: Node[], id: string): Node[] {
  const byId = new Map(nodes.map((node) => [node.id, node]));
  const chain: Node[] = [];
  let currentParent = byId.get(id)?.parentId ?? null;
  while (currentParent) {
    const parent = byId.get(currentParent);
    if (!parent) break;
    chain.unshift(parent);
    currentParent = parent.parentId;
  }
  return chain;
}

export function pathLabel(nodes: Node[], id: string): string {
  const byId = new Map(nodes.map((node) => [node.id, node]));
  const self = byId.get(id);
  if (!self) return "";
  return [...ancestorsOf(nodes, id), self].map((node) => node.name).join(" / ");
}
