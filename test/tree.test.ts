import { describe, expect, it } from "vitest";
import type { Node } from "../src/types";
import {
  flattenVisible,
  nextFocusAfterDelete,
  resolveCreateTarget,
  subtreeCounts,
  subtreeIds,
} from "../src/explorer/tree";

const nodes: Node[] = [
  { id: "f1", parentId: null, kind: "folder", name: "Folder A" },
  { id: "f2", parentId: "f1", kind: "folder", name: "Nested" },
  { id: "s1", parentId: "f1", kind: "session", name: "Session One" },
  { id: "s2", parentId: "f2", kind: "session", name: "Session Two" },
  { id: "m1", parentId: "f1", kind: "material", name: "Notes.md" },
  { id: "s3", parentId: null, kind: "session", name: "Root Session" },
];

describe("subtreeIds", () => {
  it("includes the node itself and every descendant, matching the backend's recursive query", () => {
    expect(new Set(subtreeIds(nodes, "f1"))).toEqual(new Set(["f1", "f2", "s1", "s2", "m1"]));
  });
  it("returns just the node itself for a leaf", () => {
    expect(subtreeIds(nodes, "s3")).toEqual(["s3"]);
  });
});

describe("subtreeCounts", () => {
  it("counts descendants by kind, excluding the node itself", () => {
    expect(subtreeCounts(nodes, "f1")).toEqual({ folders: 1, sessions: 2, materials: 1 });
  });
  it("is all zeros for a leaf or an empty folder", () => {
    expect(subtreeCounts(nodes, "s3")).toEqual({ folders: 0, sessions: 0, materials: 0 });
    expect(subtreeCounts(nodes, "f2")).toEqual({ folders: 0, sessions: 1, materials: 0 });
  });
});

describe("resolveCreateTarget", () => {
  it("resolves a folder to itself", () => {
    expect(resolveCreateTarget(nodes[0]!)).toBe("f1");
  });
  it("resolves a session or material to its parent", () => {
    expect(resolveCreateTarget(nodes[2]!)).toBe("f1");
    expect(resolveCreateTarget(nodes[4]!)).toBe("f1");
  });
  it("resolves a root session/material's parent, and null focus, to root", () => {
    expect(resolveCreateTarget(nodes[5]!)).toBeNull();
    expect(resolveCreateTarget(null)).toBeNull();
  });
});

describe("nextFocusAfterDelete", () => {
  const expanded = new Set(["f1", "f2"]);
  const flat = flattenVisible(nodes, expanded, "en");

  it("focuses the row that followed the deleted one", () => {
    // Flattened order: f1, f2, s2, m1, s1, s3 - deleting f2 should focus s2.
    expect(nextFocusAfterDelete(flat, "f2")).toBe("s2");
  });
  it("falls back to the previous row when the deleted row was last", () => {
    expect(nextFocusAfterDelete(flat, "s3")).toBe("s1");
  });
  it("falls back to root (null) when the tree becomes empty", () => {
    const single = [{ id: "only", parentId: null, kind: "session", name: "Only" } as Node];
    const singleFlat = flattenVisible(single, new Set(), "en");
    expect(nextFocusAfterDelete(singleFlat, "only")).toBeNull();
  });
  it("returns null when the id is not in the flattened list", () => {
    expect(nextFocusAfterDelete(flat, "missing")).toBeNull();
  });
});
