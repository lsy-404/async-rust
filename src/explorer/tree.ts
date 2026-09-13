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

// Includes the node itself, matching the backend's recursive subtree query.
export function subtreeIds(nodes: Node[], id: string): string[] {
  const byParent = childrenByParent(nodes);
  const out: string[] = [];
  const walk = (nodeId: string) => {
    out.push(nodeId);
    for (const child of byParent.get(nodeId) ?? []) walk(child.id);
  };
  walk(id);
  return out;
}

export interface SubtreeCounts {
  folders: number;
  sessions: number;
  materials: number;
}

// Excludes the node itself, for the delete-confirmation copy.
export function subtreeCounts(nodes: Node[], id: string): SubtreeCounts {
  const byParent = childrenByParent(nodes);
  const counts: SubtreeCounts = { folders: 0, sessions: 0, materials: 0 };
  const walk = (nodeId: string) => {
    for (const child of byParent.get(nodeId) ?? []) {
      if (child.kind === "folder") counts.folders += 1;
      else if (child.kind === "session") counts.sessions += 1;
      else counts.materials += 1;
      walk(child.id);
    }
  };
  walk(id);
  return counts;
}

// False when the target parent is the dragged node itself, one of its
// descendants (a cycle, also rejected server-side by the nodes_no_cycle
// trigger) or its current parent (a no-op move).
export function isValidMoveTarget(
  nodes: Node[],
  dragId: string,
  targetParentId: string | null,
): boolean {
  const dragNode = nodes.find((node) => node.id === dragId);
  if (!dragNode) return false;
  if (targetParentId === dragNode.parentId) return false;
  if (targetParentId !== null && subtreeIds(nodes, dragId).includes(targetParentId)) {
    return false;
  }
  return true;
}

// A folder resolves to that folder; a session or material resolves to its
// parent; null (root focused or nothing focused) resolves to root.
export function resolveCreateTarget(focused: Node | null): string | null {
  if (!focused) return null;
  if (focused.kind === "folder") return focused.id;
  return focused.parentId;
}

// Focus target after a delete: the row that followed the deleted one in the
// pre-delete flattened list, else the previous row, else root (null).
export function nextFocusAfterDelete(flat: FlatRow[], id: string): string | null {
  const index = flat.findIndex((row) => row.node.id === id);
  if (index === -1) return null;
  if (index + 1 < flat.length) return flat[index + 1]!.node.id;
  if (index > 0) return flat[index - 1]!.node.id;
  return null;
}
