import assert from "node:assert/strict";
import test from "node:test";
import { collectRecentTurns } from "./voice-input-session-registry.ts";

// All fixtures are synthetic; no installed extension or session files are read.
const text = (value) => ({ type: "text", text: value });
const message = (role, content, extra = {}) => ({
  type: "message",
  message: { role, content, ...extra },
});
const user = (content) => message("user", content);
const assistant = (content, stopReason = "stop") =>
  message("assistant", content, { stopReason });

function assertBounded(turns) {
  for (const turn of turns) {
    assert.ok(turn.user.length <= 12_000);
    assert.ok(turn.assistant.length <= 12_000);
  }
}

test("collects string and text blocks, ignoring nontext content and nonconversation entries", () => {
  const ignoredContent = [
    { type: "image", data: "synthetic-image", mimeType: "image/png" },
    { type: "thinking", thinking: "hidden reasoning" },
    { type: "toolCall", name: "read", arguments: { path: "synthetic" } },
  ];
  assert.deepEqual(collectRecentTurns([
    message("system", "system instructions"),
    { type: "custom_message", content: "custom content" },
    { type: "compaction", summary: "compaction content" },
    { type: "branch_summary", summary: "abandoned branch content" },
    user([...ignoredContent, text("Explain synthetic parser")]),
    message("toolResult", [text("tool output")]),
    message("custom", [text("background task finished")]),
    message("bashExecution", "bash output"),
    assistant([text("Parser answer"), ...ignoredContent, text("Second paragraph")]),
    user("Next synthetic question"),
    assistant("String answer"),
  ]), [
    { user: "Explain synthetic parser", assistant: "Parser answer\nSecond paragraph" },
    { user: "Next synthetic question", assistant: "String answer" },
  ]);
});

test("keeps the last five rounds in oldest-to-newest order", () => {
  const branch = Array.from({ length: 6 }, (_, index) => [
    user(`Question ${index + 1}`), assistant([text(`Answer ${index + 1}`)]),
  ]).flat();
  assert.deepEqual(collectRecentTurns(branch), Array.from({ length: 5 }, (_, index) => ({
    user: `Question ${index + 2}`,
    assistant: `Answer ${index + 2}`,
  })));
});

test("retains the latest user-only round and selects five before text filtering", () => {
  const branch = [user("Excluded older question"), assistant("Excluded older answer")];
  for (let index = 1; index <= 4; index += 1) {
    branch.push(user(`Question ${index}`), assistant(`Answer ${index}`));
  }
  // Whitespace text is still a user turn before extraction trims it.
  branch.push(user(" \n "));
  const turns = collectRecentTurns(branch);
  assert.equal(turns.length, 5);
  assert.equal(turns[0].user, "Question 1");
  assert.deepEqual(turns.at(-1), { user: "", assistant: "" });
  assert.deepEqual(collectRecentTurns([user("Unanswered question")]), [
    { user: "Unanswered question", assistant: "" },
  ]);
});

test("ignores orphan assistants and every nonfinal stop reason", () => {
  const branch = [assistant("Orphan answer"), user("Question")];
  for (const reason of ["toolUse", "aborted", "error", "length", "pending", "deferred", undefined]) {
    branch.push(message("assistant", [text(`Excluded ${reason}`)], { stopReason: reason }));
  }
  branch.push(assistant([text("Completed answer")]));
  assert.deepEqual(collectRecentTurns(branch), [
    { user: "Question", assistant: "Completed answer" },
  ]);
});

test("appends all completed assistant texts within the same round", () => {
  assert.deepEqual(collectRecentTurns([
    user("Explain synthetic indexing"),
    assistant([text("Substantial indexing explanation."), text("More explanation.")]),
    { type: "custom_message", content: "Synthetic background completion" },
    message("custom", "Another background notification"),
    assistant("Background work acknowledged."),
  ]), [{
    user: "Explain synthetic indexing",
    assistant: "Substantial indexing explanation.\nMore explanation.\nBackground work acknowledged.",
  }]);
});

test("publishes an empty array for an empty branch or no user message", () => {
  assert.deepEqual(collectRecentTurns([]), []);
  assert.deepEqual(collectRecentTurns([
    assistant("Orphan"),
    message("toolResult", [text("Not user text")]),
    { type: "custom", message: { role: "user", content: "Not a message entry" } },
    { type: "custom_message", content: "Not a user entry" },
    message("custom", "Not a user role"),
  ]), []);
});

test("image-only user input retains its own round without publishing image data", () => {
  assert.deepEqual(collectRecentTurns([
    user("Previous question"), assistant("Previous answer"),
    user([{ type: "image", data: "synthetic-image" }]),
    assistant("Image explanation"),
  ]), [
    { user: "Previous question", assistant: "Previous answer" },
    { user: "", assistant: "Image explanation" },
  ]);
});

test("uses only the supplied active branch, ignoring alternative branch summaries", () => {
  const shared = [user("Shared question"), assistant("Shared answer")];
  const abandoned = [...shared, user("Abandoned question"), assistant("Abandoned answer")];
  const active = [
    ...shared,
    { type: "branch_summary", summary: "Abandoned question and answer" },
    user("Active question"),
    assistant("Active answer"),
  ];
  assert.equal(collectRecentTurns(abandoned).at(-1).user, "Abandoned question");
  assert.deepEqual(collectRecentTurns(active), [
    { user: "Shared question", assistant: "Shared answer" },
    { user: "Active question", assistant: "Active answer" },
  ]);
});

test("caps each raw role at whole lines, omitting oversized sensitive lines in full", () => {
  const sensitiveLine = `api_key=synthetic_${"S".repeat(40_000)}_sensitive_suffix`;
  const source = `Safe introductory line\n${sensitiveLine}\nSafe final line`;
  const turns = collectRecentTurns([user(source), assistant([text(source)])]);
  assertBounded(turns);
  for (const value of Object.values(turns[0])) {
    assert.equal(value, "Safe introductory line\n…\nSafe final line");
    assert.ok(!value.includes("sensitive_suffix"));
    assert.ok(!value.includes("SSSS"));
  }
  assert.deepEqual(collectRecentTurns([user(sensitiveLine), assistant(sensitiveLine)]), [
    { user: "…", assistant: "…" },
  ]);
});

test("caps combined assistant messages rather than letting the last acknowledgement replace the answer", () => {
  const branch = [user("Synthetic explanation"), assistant("Substantial answer opening")];
  for (let index = 0; index < 100; index += 1) {
    branch.push(assistant(`Detail ${index}: ${"D".repeat(300)}`));
  }
  branch.push(assistant("Final background acknowledgement"));
  const turns = collectRecentTurns(branch);
  assertBounded(turns);
  assert.ok(turns[0].assistant.startsWith("Substantial answer opening\n"));
  assert.ok(turns[0].assistant.endsWith("\nFinal background acknowledgement"));
  for (const line of turns[0].assistant.split("\n")) {
    assert.ok(line === "…" || line === "Substantial answer opening"
      || line === "Final background acknowledgement" || /^Detail \d+: D{300}$/.test(line));
  }
});

test("preserves small sensitive lines for Rust redaction and never splits Unicode lines", () => {
  const sensitive = "api_key=synthetic_short_credential";
  assert.deepEqual(collectRecentTurns([user(sensitive), assistant(sensitive)]), [
    { user: sensitive, assistant: sensitive },
  ]);
  const line = "\u{1D400}".repeat(99);
  const source = Array.from({ length: 100 }, () => line).join("\n");
  const turns = collectRecentTurns([user(source), assistant(source)]);
  assertBounded(turns);
  for (const value of Object.values(turns[0])) {
    assert.ok(value.isWellFormed());
    assert.ok(value.split("\n").every((retained) => retained === line || retained === "…"));
  }
});
