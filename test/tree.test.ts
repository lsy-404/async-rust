import { describe, expect, it } from "vitest";
import type { Node } from "../src/types";
import {
  flattenVisible,
  isValidMoveTarget,
  moveDestinations,
  nextFocusAfterDelete,
  resolveCreateTarget,
  subtreeCounts,
  subtreeIds,
  timestampName,
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

describe("isValidMoveTarget", () => {
  it("refuses dropping a folder into itself", () => {
    expect(isValidMoveTarget(nodes, "f1", "f1")).toBe(false);
  });
  it("refuses dropping a folder into its own descendant", () => {
    expect(isValidMoveTarget(nodes, "f1", "f2")).toBe(false);
  });
  it("refuses a no-op drop onto the current parent", () => {
    expect(isValidMoveTarget(nodes, "s1", "f1")).toBe(false);
    expect(isValidMoveTarget(nodes, "s3", null)).toBe(false);
  });
  it("allows dropping into an unrelated folder or to root", () => {
    expect(isValidMoveTarget(nodes, "s1", "f2")).toBe(true);
    expect(isValidMoveTarget(nodes, "f2", null)).toBe(true);
  });
  it("refuses an unknown dragged id", () => {
    expect(isValidMoveTarget(nodes, "missing", "f1")).toBe(false);
  });
});

describe("moveDestinations", () => {
  it("offers root plus every unrelated folder, labelled by full path, excluding the current parent", () => {
    expect(moveDestinations(nodes, "s1", "(Root)")).toEqual([
      { parentId: null, label: "(Root)" },
      { parentId: "f2", label: "Folder A / Nested" },
    ]);
  });
  it("excludes root when the node is already there, and excludes the node's own subtree", () => {
    // f1 is already at root (no root option), and f2 is inside f1's own
    // subtree (a cycle), so nothing is offered.
    expect(moveDestinations(nodes, "f1", "(Root)")).toEqual([]);
  });
  it("excludes root for a node already at root, but still offers every folder", () => {
    expect(moveDestinations(nodes, "s3", "(Root)")).toEqual([
      { parentId: "f1", label: "Folder A" },
      { parentId: "f2", label: "Folder A / Nested" },
    ]);
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

describe("timestampName", () => {
  it("formats as YYYY-MM-DD HH:mm:ss in local time, zero-padded", () => {
    expect(timestampName(new Date(2024, 2, 5, 9, 3, 7))).toBe("2024-03-05 09:03:07");
  });
  it("pads a single-digit month, day, hour, minute and second all at once", () => {
    expect(timestampName(new Date(2024, 0, 1, 0, 0, 0))).toBe("2024-01-01 00:00:00");
  });
});
