import { cpSync, mkdirSync } from "node:fs";
import { join } from "node:path";

const root = new URL("../desktop-host/", import.meta.url).pathname;
const output = process.argv[2] ?? join(root, "dist");
mkdirSync(output, { recursive: true });
cpSync(root, join(output, "desktop-host"), { recursive: true, filter: (source) => !source.includes("/dist") });
