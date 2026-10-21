import { test } from "node:test";
import assert from "node:assert/strict";
import { slugify } from "../src/slugify.js";

test("spaces become dashes", () => assert.equal(slugify("Hello World"), "hello-world"));
test("punctuation is dropped and runs collapse", () =>
  assert.equal(slugify("  Stack: agents & worktrees!! "), "stack-agents-worktrees"));
test("accents are folded", () => assert.equal(slugify("Crème Brûlée"), "creme-brulee"));
