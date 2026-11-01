# Issue tracker: agent-todo

Issues and specs for this repo live in **agent-todo**, reached through the `agent-todo` MCP server. There is no GitHub or GitLab tracker.

- **Project code**: `LRM` — from `AGENTS.md` and `.agent-todo/config.toml`.
- **First group**: `LRM-1` (project code `-1`) — the project-level context.

## The model

Two things live in agent-todo, and the distinction matters:

- **Spec** = a **group's context**. Each feature gets its own group; the spec body lives in that group's context sections. Do **not** put feature specs in the first group (`LRM-1`) — that group is reserved for project-level logic.
- **Ticket** = a **task** inside that group. Tickets are what you create, list, claim, and close through the task tools.

Group ids look like `<project>-<n>` (e.g. `LRM-1`, `LRM-2`). `group_list` and `group_create` take the project code (`LRM`); the `task_*` and `context_*` tools take a group id or task id.

## Conventions

- **Read project context**: `context_get { group: "LRM-1" }`. Holds decisions, planning, architecture, and each module's behavior — logic only, no code or pseudocode. The commands section may hold the exact build and test commands.
- **Record a project decision**: `context_update { group: "LRM-1", ops: [ ... ] }`.
- **Create a feature group (spec)**: `group_create { project: "LRM", name: "<feature>" }`. Keep the returned group id.
- **Publish the spec**: `context_update { group: "<gid>", ops: [{ op: "upsert", slug: "<slug>", title: "<title>", body: "<markdown>" }] }`. Sections may be marked `always_include`, or gated by `applies_to` tags so they only surface for matching tickets. Other ops: `append`, `replace_text`, `delete`, `move`.
- **Read the spec**: `context_get { group: "<gid>" }` (optionally filter with `sections`).
- **Create tickets**: `task_create { group: "<gid>", tasks: [{ title, description, priority, tags, acceptance: [...], depends_on: [...], parent }] }`. Batches of up to 50.
- **Read a ticket**: `task_get { id: "<id>", include: ["comments", "deps", "history", "context"] }`. The user normally passes the ticket id alone.
- **List tickets**: `task_list { group: "<gid>", status: [...], tags: [...], assignee: "...", q: "...", sort: "priority" | "updated" }`.
- **Claim the next ready ticket**: `task_next { project: "LRM" }` (across the project) or `task_next { group: "<gid>" }` (stay inside one feature). Pass `claim: true` to take it. `task_claim { id }` claims one specific ticket.
- **Update a ticket**: `task_update { id, v, set: { title, description, priority, assignee }, add: { tags, skills, mcps } }`. `v` is the current version (optimistic concurrency) — read it from `task_get` first.
- **Comment**: `comment_add { task: "<id>", body: "<text>", kind: "note" | "question" | "decision" | "handoff" | "verification" | "blocker" | "summary" }`.
- **Move status**: `task_status { id, v, to: "todo" | "open" | "inprogress" | "reopen" | "verifying" | "verified" | "deploying" | "deployed", comment: { body, kind } }`.
- **Close / archive**: `task_status { ... to: "verified" }`, then `task_archive { id, reason: "<why>" }`.

## Blocking

- **Dependencies**: set `depends_on` on create (or via the task tools). A ticket is startable when its dependencies are done; `task_next` only returns ready tickets, so it already skips blocked work.
- **A blocker you discover mid-flight**: `comment_add { task: "<id>", kind: "blocker", body: "<what is blocked by what>" }`.

## Triage

There is no separate label vocabulary here: a ticket's triage state is its **`status`** plus its **`tags`** (tags also select which context sections apply via `applies_to`). Filter on both with `task_list`.

## When a skill says "publish to the issue tracker"

Create (or reuse) the feature group with `group_create { project: "LRM", name: "<feature>" }`, upsert the spec into that group's context with `context_update`, and create the tickets as tasks in the same group with `task_create`.

## When a skill says "fetch the relevant ticket"

`task_get { id: "<id>" }`. Add `include: ["context"]` to pull the spec sections that apply to that ticket's tags, and `include: ["comments", "deps"]` for its discussion and blockers.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a group's context; **child tickets** are tasks in that group.

- **Map**: `group_create { project: "LRM", name: "<effort>" }`, whose context holds the Notes / Decisions-so-far / Fog body.
- **Child ticket**: `task_create { group: "<gid>", tasks: [{ title, tags: ["<type>"] }] }`, where type is `research` / `prototype` / `grilling` / `task`.
- **Blocking**: dependency edges via `depends_on`.
- **Frontier**: the map's open, unblocked, unclaimed tickets — `task_next { group: "<gid>", claim: true }` picks and claims the first.
- **Claim**: `task_next { group: "<gid>", claim: true }` (or `task_claim { id }`) — the session's first write. The claimant becomes the assignee.
- **Resolve**: `comment_add { task: "<id>", kind: "summary", body: "<answer>" }`, then `task_status` to the closing status, then append a context pointer (gist + link) to the map's Decisions-so-far with `context_update`.
