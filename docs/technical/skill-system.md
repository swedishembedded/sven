# Skill System

Skills are self-contained instruction packages that extend the agent's behaviour
for a specific domain.  This document covers the complete internal architecture:
how skills are stored, discovered, parsed, assembled into the system prompt, and
loaded on demand by the model.

---

## Concepts

| Term | Definition |
|------|-----------|
| **Skill package** | A directory containing a `SKILL.md` file |
| **Command** | The slash-command key for a skill, derived from its directory path (e.g. `"sven/plan"`) |
| **Sub-skill** | A skill package nested inside another skill package |
| **Display name** | Human-readable label from the `name:` frontmatter field |
| **Body** | Everything in `SKILL.md` after the closing `---` fence |

---

## Directory layout

Every skills source is a flat root that may contain arbitrarily nested skill
packages.  The skill's **command** is the slash-separated path relative to the
root:

```
<skills-root>/
├── git-workflow/
│   └── SKILL.md              command: "git-workflow"
└── sven/
    ├── SKILL.md              command: "sven"
    ├── scripts/              (no SKILL.md → not a sub-skill; bundled files only)
    │   └── helper.sh
    ├── plan/
    │   └── SKILL.md          command: "sven/plan"
    ├── implement/
    │   ├── SKILL.md          command: "sven/implement"
    │   └── research/
    │       └── SKILL.md      command: "sven/implement/research"
    └── review/
        └── SKILL.md          command: "sven/review"
```

A directory that contains only non-`SKILL.md` files (`scripts/`, `docs/`,
`references/`) is treated as **bundled support files** for its parent skill —
it is not registered as a sub-skill.

---

## Discovery sources

`discover_skills()` in `sven-runtime` scans six directories in order of
increasing precedence.  When two sources contain a skill with the same command,
the later (higher-precedence) source wins:

| Priority | Directory | Scope |
|----------|-----------|-------|
| 1 (lowest) | `~/.sven/skills/` | User-global sven skills |
| 2 | `~/.agents/skills/` | User-global cross-agent skills |
| 3 | `~/.claude/skills/` | User-global Claude Code compatibility |
| 4 | `<project>/.agents/skills/` | Project cross-agent skills |
| 5 | `<project>/.claude/skills/` | Project Claude Code compatibility |
| 6 (highest) | `<project>/.sven/skills/` | Project sven skills |

Each source is scanned with `scan_skills_dir`, which calls `scan_recursive`
to walk the directory tree.

---

## Recursive scanner

```
scan_skills_dir(root, source)
  └── scan_recursive(root, root, source, &mut out)
        for each subdirectory child of current dir:
          if child/SKILL.md exists:
            command = child.strip_prefix(root)  # e.g. "sven/plan"
            try_load_skill(child, SKILL.md, command, source)
              → size check (256 KB cap)
              → read + parse frontmatter
              → check requires_bins / requires_env
              → build SkillInfo { command, name, description, ... }
          recurse into child (even without SKILL.md — nested sub-skills may exist)
```

Key properties of the scanner:

- **Root is not a skill.** Only directories *inside* the root are skill packages.
- **Non-skill directories are traversed.** A directory without `SKILL.md` is still
  descended into, so a skill at `sven/implement/research` is found even when
  `sven/implement/` does not contain a `SKILL.md` itself.
- **No maximum depth.** Nesting can be arbitrarily deep.

---

## SKILL.md format

```markdown
---
description: |
  When to use this skill and what trigger phrases apply.
name: Human-Readable Label   # optional — falls back to directory name
version: 1.0.0               # optional semver
sven:                        # optional sven-specific block
  always: false              # always include in system prompt
  requires_bins: [docker]    # skip if these binaries are absent
  requires_env: [DOCKER_TOKEN] # skip if these env vars are unset
  user_invocable_only: false # hide from model; show only as slash command
---

# Skill body

Instructions the model will follow when this skill is loaded.
Reference sub-skills with load_skill("command") calls.
```

Only `description` is required.  `name` defaults to the directory name when
omitted.  The old `sven.skills:` list (manual sub-skill declaration) has been
removed; relationships are now derived structurally from the directory tree.

---

## `SkillInfo` data type (`sven-runtime`)

```rust
pub struct SkillInfo {
    pub command:       String,         // "sven/plan"
    pub name:          String,         // "Sven Plan" (from frontmatter or dir name)
    pub description:   String,         // frontmatter description
    pub version:       Option<String>, // frontmatter version
    pub skill_md_path: PathBuf,        // /…/sven/plan/SKILL.md
    pub skill_dir:     PathBuf,        // /…/sven/plan/
    pub content:       String,         // body after the closing ---
    pub sven_meta:     Option<SvenSkillMeta>,
}
```

`SvenSkillMeta` carries availability guards (`requires_bins`, `requires_env`),
the `always` and `user_invocable_only` flags.

---

## System prompt injection (`sven-core`)

At startup, `build_skills_section()` serialises the discovered skills into an
XML block that is appended to the system prompt:

```xml
## Skills

When you recognize that the current task matches one of the available skills
listed below, call the `load_skill` tool to load the full skill instructions
before proceeding. …

<available_skills>
  <skill>
    <command>sven</command>
    <name>Sven Methodology</name>
    <description>Use when the user asks to work on a task …</description>
  </skill>
  <skill>
    <command>sven/plan</command>
    <name>Sven Plan</name>
    <description>Use this skill for the planning phase …</description>
  </skill>
  …
</available_skills>
```

Only metadata (command, name, description) is injected — never the body.  This
keeps the system prompt lean and token usage proportional to what the session
actually needs.

**Character budget**: `MAX_SKILLS_PROMPT_CHARS` (30 000 characters) caps the
total size of the `<available_skills>` block.  Skills with `always: true`
bypass the cap; the remaining candidates are packed in discovery order until the
budget would be exceeded.  A truncation notice is appended when any skills are
left out.

**`user_invocable_only: true`** hides a skill from the model's list but still
registers it as a TUI slash command.  The model will never call it
autonomously; the user can invoke it explicitly with `/command`.

---

## On-demand loading: `LoadSkillTool` (`sven-tools`)

When the model decides a skill is relevant it calls `load_skill`:

```
load_skill({"name": "sven/plan"})
```

The tool returns a `<skill_content>` block containing:

1. **Full body** — the complete SKILL.md body (no frontmatter).
2. **Base directory** — absolute path to the skill directory, so that relative
   references to `scripts/`, `references/`, etc. can be resolved with
   `read_file`.
3. **Bundled files listing** — up to 20 file paths from the skill directory,
   excluding the SKILL.md itself and any sub-skill subdirectories (they are
   separate packages).
4. **Sub-skill navigation hint** — a compact `<sub_skills>` block listing the
   skill's **direct children** (one level only) by command and one-line
   description.  The model calls `load_skill` again for whichever child it
   needs next.

Example output for `load_skill("sven")`:

```xml
<skill_content command="sven" name="Sven Methodology">
# Skill: Sven Methodology

… full body …

Base directory: /path/to/.sven/skills/sven
Relative paths in this skill (scripts/, references/, assets/) are relative to
this base directory.

<sub_skills>
<!-- Call load_skill(command) to load any sub-skill's full instructions. -->
  <sub_skill command="sven/plan" name="Sven Plan">Planning phase.</sub_skill>
  <sub_skill command="sven/implement" name="Sven Implement">Implementation phase.</sub_skill>
  <sub_skill command="sven/review" name="Sven Review">Review phase.</sub_skill>
</sub_skills>
</skill_content>
```

The sub-skills hint is constructed by matching skills whose command starts with
`parent.command + "/"` and whose remainder contains no further `/` (direct
children only).  Grandchildren are not listed at the parent level; they appear
in the child's own hint when that child is loaded.

---

## TUI slash commands (`sven-tui`)

At TUI startup, `App::new()` calls `discover_skills()` and passes the slice to
`register_skills()`, which converts each `SkillInfo` into a `SkillCommand` via
`make_skill_commands()`.

Each `SkillCommand`:
- has `name` = sanitized command path (e.g. `"sven/plan"`), preserving `/`
- stores the full SKILL.md body
- when executed, sends the body — optionally followed by a user task — as the
  next agent message

The sanitizer converts each `/`-separated path segment individually: spaces and
hyphens become `_`, uppercase is lowercased, consecutive non-alnum runs are
collapsed.  The `/` separator is preserved so the TUI sees `sven/plan` as one
command with two levels.

Example:

```
User types:  /sven/plan analyse the authentication module
SkillCommand receives args: ["analyse", "the", "authentication", "module"]
Message sent:
  <full sven/plan SKILL.md body>

  Task: analyse the authentication module
```

Sub-skill bodies are never pre-loaded.  Only the invoked skill's own body is
sent.  The model discovers and loads children via the sub-skill hint returned by
`load_skill`.

---

## Crate responsibilities

| Crate | Responsibility |
|-------|---------------|
| `sven-runtime` | `SkillInfo`, `SvenSkillMeta`, `ParsedSkill`; `parse_skill_file()`; `discover_skills()` and the recursive scanner; requirement checking (`requires_bins`, `requires_env`) |
| `sven-core` | `build_skills_section()` — serialises skill metadata into the system-prompt XML block; `PromptContext.skills` field |
| `sven-tools` | `LoadSkillTool` — tool implementation, child-detection logic, bundled-file collection |
| `sven-bootstrap` | Calls `discover_skills()`, stores the `Arc<[SkillInfo]>` in `RuntimeContext`, wires it into `AgentRuntimeContext` and registers `LoadSkillTool` |
| `sven-tui` | `SkillCommand`, `make_skill_commands()`, `sanitize_command_name()`; `register_skills()` in `CommandRegistry`; wires discovery into `App::new()` |

---

## Domain knowledge convention

Skills and agents covering complex subsystems benefit from embedded domain
knowledge — not just procedural instructions, but project-specific facts,
correctness invariants, and known failure modes.

### Recommended sections for domain-rich skills

When writing a skill (or agent spec) for a complex subsystem, include the
following sections after the procedural instructions:

```markdown
## Domain Knowledge

### Correctness Invariants
- List the properties that must always hold.  Frame as "MUST" / "MUST NOT".
- Example: "All relay pathspecs MUST use the `:(glob)` prefix on non-Linux platforms."

### Known Failure Modes
| Symptom | Cause | Fix |
|---------|-------|-----|
| Connection drops after 30 s | KEEP_ALIVE not sent | Enable `SwarmConfig::keep_alive` |
| mDNS silent in Docker | Multicast routing disabled | Use relay-only discovery mode |

### Critical Patterns
- Specific code patterns that must be followed and why.
- Example: "Always call `Swarm::dial()` with a `DialOpts::peer_id()` guard to avoid duplicate dials."
```

### Relationship with the knowledge base

The knowledge base (`.sven/knowledge/`) stores *project-level* specifications
that any agent can retrieve.  Skills and agents can cross-reference them with
an optional `knowledge:` frontmatter field:

```markdown
---
name: p2p-specialist
description: P2P networking expert. Use when modifying sven-p2p or sven-node.
knowledge:
  - sven-p2p.md
  - sven-node.md
---

… procedural instructions …

## Domain Knowledge

… embedded domain facts that are always needed …
```

When an agent spec declares `knowledge:` files, `load_skill` appends a hint:

```
Relevant knowledge docs (use search_knowledge or read_file to load):
  - .sven/knowledge/sven-p2p.md  (P2P Networking)
  - .sven/knowledge/sven-node.md (Node Gateway)
```

**Guideline:** embed the core correctness invariants and most-common failure
modes directly in the skill/agent spec (they are always needed), and link to
the knowledge base for the full architecture narrative and extended failure
tables (loaded on demand).

---

## Token efficiency design

Token efficiency is a first-class concern:

- **Metadata only in system prompt.** The `<available_skills>` block carries
  command, name, and description — never the body.  A typical body is 300–2000
  tokens; keeping only metadata saves the vast majority of that cost.
- **Body loaded on demand.** `load_skill` is called at most once per skill per
  session, and only when the model judges it necessary.
- **Sub-skill hint, not body.** When a parent skill is loaded, its children are
  listed as one-liner hints.  The full child body is loaded only if the model
  decides it is needed for the current task.
- **Grandchildren not listed at parent level.** Each level of the hierarchy
  returns only its immediate children, preventing deep trees from bloating any
  single tool call's response.
- **Character budget on system prompt.** The 30 000-character cap prevents a
  large skill library from consuming the model's thinking budget before the
  conversation starts.
