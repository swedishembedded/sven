# Skills and project instructions

## Skills

Skills are instruction packages that teach sven how to handle a specific type
of task.  Each skill lives in its own directory alongside any helper scripts or
reference files it needs.  When you invoke a skill, sven loads its instructions
and follows them for your task.

Skills are discovered automatically from your project and home directory on
startup.  You do not need to configure anything; drop a skill directory in the
right place and it appears immediately.

---

### Using a skill

Skills are not registered as slash commands. The model sees every discovered
skill's name and description in its system prompt and loads the matching one
with the `skill` tool when your request fits. To use a specific skill, name it
in your message:

```
Use the sven/plan skill to analyse the authentication module
```

For a reusable `/command`, put a markdown file in a `commands/` directory
instead (see [Custom commands](interactive.md#custom-commands)).

---

### Hierarchical skills

Skills can be nested.  A top-level skill like `sven` describes a high-level
workflow and lists the sub-skills that handle each phase.  Each sub-skill is
also a skill in its own right, loadable by its command (`sven/plan`,
`sven/implement`, `sven/review`).

When the model loads a parent skill, it receives a compact list of the
available sub-skills.  It then loads `sven/plan` etc. with the `skill` tool exactly
when it enters each phase - not before.  This means sub-skill instructions are
loaded only when actually needed, keeping each turn's token usage minimal.

---

### Where to put skills

Sven collects skills from the system directories and from every ancestor of
your project and of your home directory. When the same command exists in more
than one place, the one nearer the project wins, and within one directory
`.sven/` wins over the others:

| Location | When to use |
|----------|-------------|
| `/usr/share/sven/skills/`, `/usr/local/share/sven/skills/` | Skills installed for every user (lowest precedence) |
| `~/.sven/skills/` | Your personal skills, available in every project |
| `~/.agents/skills/`, `~/.claude/skills/`, `~/.codex/skills/`, `~/.cursor/skills/` | Skills shared with other agents, personal |
| `<project>/.agents/skills/`, `.claude/skills/`, `.codex/skills/`, `.cursor/skills/` | Project skills shared with other agents |
| `<project>/.sven/skills/` | Skills specific to this project (highest precedence) |

A skills directory in a parent of the project (for example a workspace that
holds several checkouts) is found too, with lower precedence than the project's
own. `/refresh` in the TUI re-scans without a restart; `/skills` opens the
inspector.

---

### Creating a skill

A skill is a directory containing a `SKILL.md` file:

```
.sven/skills/
└── deploy/
    ├── SKILL.md
    └── scripts/
        └── pre-flight.sh
```

`SKILL.md` starts with a YAML frontmatter block followed by the instruction
body:

```markdown
---
description: |
  Use this skill when the user asks to deploy, release, or ship the application.
  Trigger phrases: "deploy", "release", "ship to production".
name: Deploy             # optional - defaults to directory name
version: 1.0.0           # optional
---

# Deploy

Before deploying, run the pre-flight checklist in scripts/pre-flight.sh.

1. Confirm the target environment with the user.
2. Run `scripts/pre-flight.sh` and fix any failures.
3. Build the release artefact.
4. Push and tag.
```

The `description` field is the only required frontmatter key.  The model reads
it to decide whether to use the skill, so write it as a list of trigger phrases
and use-cases rather than a technical summary.

---

### Creating a hierarchical skill

Nest sub-skill directories inside the parent:

```
.sven/skills/
└── deploy/
    ├── SKILL.md              command: deploy
    ├── pre-flight/
    │   └── SKILL.md          command: deploy/pre-flight
    └── rollback/
        └── SKILL.md          command: deploy/rollback
```

In the parent `SKILL.md`, tell the model to load sub-skills at the right time:

```markdown
---
description: |
  Full deployment workflow. Use when deploying to any environment.
---

# Deploy Workflow

Follow these phases in order:

1. Pre-flight checks - load the `deploy/pre-flight` skill before touching
   any infrastructure.
2. Deploy the artefact.
3. If anything fails - load the `deploy/rollback` skill immediately.
```

Sub-skills are automatically listed to the model when the parent is loaded, so
you do not need to declare them in the frontmatter.  Just create the directory.

---

### Frontmatter reference

| Key | Type | Required | Description |
|-----|------|----------|-------------|
| `description` | string | **yes** | Trigger phrases and use-cases. The model matches this against the user's request. |
| `name` | string | no | Human-readable label shown in the UI. Defaults to the directory name. |
| `version` | string | no | Semver version string for your own tracking. |
| `sven.always` | bool | no | Always include this skill's metadata in the system prompt, regardless of the token budget. Useful for a project-wide coding-style skill. Default: `false`. |
| `sven.requires_bins` | list | no | Skip this skill if any of the listed binaries are absent from `PATH` (e.g. `[docker, kubectl]`). |
| `sven.requires_env` | list | no | Skip this skill if any of the listed environment variables is unset or empty (e.g. `[AWS_PROFILE]`). |
| `sven.user_invocable_only` | bool | no | Hide the skill from the model's skill listings, so it is loaded only when you name it. Use for skills you always want to invoke deliberately. Default: `false`. |
| `sven.activation` | map | no | List the skill to the model only once it applies: `paths` (globs of files touched or named), `modes`, `states`, or `always`. Absent means always. See [instruction disclosure](../internals/instruction-disclosure.md). |

The `sven.*` keys are fields of a `sven:` block:

```yaml
sven:
  always: true
  requires_bins: [docker]
```

A file with no frontmatter fence is accepted as plain Markdown: its first
non-heading line becomes the description.

---

### Bundled files

Any file in a skill directory that is not a `SKILL.md` is a **bundled file** -
a script, reference document, template, or data file the skill's instructions
may use.  Subdirectories without their own `SKILL.md` are support directories,
not sub-skills.

When a skill is loaded, the agent receives a listing of up to
20 bundled file paths relative to the skill directory.  The skill body can
reference them:

```markdown
Run the helper at `scripts/validate.py` before proceeding.
```

The agent resolves that path against the base directory shown in the tool
response and reads the file with `read_file`.

---

### Tips

**Write descriptions as trigger phrases.**  The model matches the description
against what the user asked.  `"Use when deploying to production"` is more
useful than `"Deployment skill"`.

**Keep parent bodies short.**  The parent skill should describe the workflow
and tell the model *when* to call each sub-skill.  Put the detailed instructions
in the sub-skills.

**Use `sven.always` sparingly.**  Skills marked `always` are included in every
system prompt.  Reserve this for genuinely project-wide rules (e.g. a coding
style guide) rather than task-specific workflows.

**Use `sven.user_invocable_only` for personal workflows.**  If a skill
contains steps you always want to review before running (e.g. a production
deployment), mark it `user_invocable_only` so the model never triggers it
automatically.

**Override globals with project skills.**  A project-level skill at
`.sven/skills/deploy/` replaces any global skill with the same command.
This lets you tailor shared skills for a specific repository.

---

## Agent routing convention

For projects with multiple specialist agents or skills, a routing table in the
project context file (`AGENTS.md` or `.sven/context.md`) encodes which domain
expert to consult before editing a given area of the codebase.

The agent reads this table as part of "Project Instructions" and follows it
automatically - no additional tooling required.

### Format

Add an `## Agent Routing` section to your `AGENTS.md`:

```markdown
## Agent Routing

Before modifying files that match the patterns below, load the indicated skill
or agent first.

| File pattern              | Before changes: load              |
|---------------------------|-----------------------------------|
| crates/hsm/**        | `/hsm-specialist`                 |
| crates/model/**      | `/model-integrator`               |
| .sven/knowledge/**        | update `updated:` date when done  |
| src/auth/**               | `/security-auditor` (readonly)    |
```

### Why it works

The routing table is written for the AI, not for humans.  When Sven sees
"Project Instructions" in its system prompt, it treats the table as binding
rules.  The model will load the skill or suggest the indicated agent before
touching matching files.

### Tips

- **Use glob patterns.** `crates/hsm/**` matches any file under that
  directory; `src/**/*.rs` matches all Rust source files.
- **Cross-reference with knowledge docs.** If a subsystem has a knowledge doc,
  note it in the routing table:
  ```markdown
  | crates/hsm/**  | `memory` action `search_knowledge` for "HSM Kernel" before changes |
  ```
- **Combine with skills.** Skills can themselves contain routing sub-tables
  for their own sub-components.
- **Start small.** A two-row routing table covering your two most error-prone
  subsystems is more effective than a twenty-row table the model ignores due
  to length.
