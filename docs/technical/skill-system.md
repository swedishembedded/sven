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
| **Command** | The key a skill is loaded by, derived from its directory path (e.g. `"sven/plan"`) |
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
`references/`) is treated as **bundled support files** for its parent skill -
it is not registered as a sub-skill.

---

## Discovery sources

`discover_skills()` in `sven-workspace` scans six directories in order of
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
          recurse into child (even without SKILL.md - nested sub-skills may exist)
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
name: Human-Readable Label   # optional - falls back to directory name
version: 1.0.0               # optional semver
sven:                        # optional sven-specific block
  always: false              # always include in system prompt
  requires_bins: [docker]    # skip if these binaries are absent
  requires_env: [DOCKER_TOKEN] # skip if these env vars are unset
  user_invocable_only: false # hide from the model's skill listings
---

# Skill body

Instructions the model will follow when this skill is loaded.
Reference sub-skills by command; the model loads them with the `skill` tool.
```

Only `description` is required.  `name` defaults to the directory name when
omitted.  The old `sven.skills:` list (manual sub-skill declaration) has been
removed; relationships are now derived structurally from the directory tree.

---

## `SkillInfo` data type (`sven-workspace`)

```rust
pub struct SkillInfo {
    pub command:       String,         // "sven/plan"
    pub name:          String,         // "Sven Plan" (from frontmatter or dir name)
    pub description:   String,         // frontmatter description
    pub version:       Option<String>, // frontmatter version
    pub skill_md_path: PathBuf,        // /.../sven/plan/SKILL.md
    pub skill_dir:     PathBuf,        // /.../sven/plan/
    pub content:       String,         // body after the closing ---
    pub sven_meta:     Option<SvenSkillMeta>,
}
```

`SvenSkillMeta` carries availability guards (`requires_bins`, `requires_env`),
the `always` and `user_invocable_only` flags.

---

## System prompt injection (`sven-turn`)

At startup, `build_skills_section()` serialises the discovered skills into an
XML block that is appended to the system prompt:

```xml
## Skills

When you recognize that the current task matches one of the available skills
listed below, call the `skill` tool with `action: "load"` and the skill's command as `name`
to load the full skill instructions
before proceeding. ...

<available_skills>
  <skill>
    <command>sven</command>
    <name>Sven Methodology</name>
    <description>Use when the user asks to work on a task ...</description>
  </skill>
  <skill>
    <command>sven/plan</command>
    <name>Sven Plan</name>
    <description>Use this skill for the planning phase ...</description>
  </skill>
  ...
</available_skills>
```

Only metadata (command, name, description) is injected - never the body.  This
keeps the system prompt lean and token usage proportional to what the session
actually needs.

**Character budget**: `MAX_SKILLS_PROMPT_CHARS` (30 000 characters) caps the
total size of the `<available_skills>` block.  Skills with `always: true`
bypass the cap; the remaining candidates are packed in discovery order until the
budget would be exceeded.  A truncation notice is appended when any skills are
left out.

**`user_invocable_only: true`** hides a skill from the model's
`<available_skills>` list and from the `skill` tool's description and
`list` results, so the model does not discover it on its own. The `skill`
tool still loads it when asked for its exact command (e.g. because the user
named it).

---

## On-demand loading: `SkillTool` (`sven-tools-agent`)

When the model decides a skill is relevant it calls the `skill` tool:

```
skill({"action": "load", "name": "sven/plan"})
```

The tool returns a `<skill_content>` block containing:

1. **Full body** - the complete SKILL.md body (no frontmatter).
2. **Base directory** - absolute path to the skill directory, so that relative
   references to `scripts/`, `references/`, etc. can be resolved with
   `read_file`.
3. **Bundled files listing** - up to 20 file paths from the skill directory,
   excluding the SKILL.md itself and any sub-skill subdirectories (they are
   separate packages).
4. **Sub-skill navigation hint** - a compact `<sub_skills>` block listing the
   skill's **direct children** (one level only) by command and one-line
   description.  The model loads whichever child it
   needs next.

Example output for loading `sven`:

```xml
<skill_content command="sven" name="Sven Methodology">
# Skill: Sven Methodology

... full body ...

Base directory: /path/to/.sven/skills/sven
Relative paths in this skill (scripts/, references/, assets/) are relative to
this base directory.

<sub_skills>
<!-- Call skill(action=load, name=<command>) to load any sub-skill's full instructions. -->
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

Sub-skill bodies are never pre-loaded.  Only the invoked skill's own body is
sent.  The model discovers and loads children via the sub-skill hint returned by
the `skill` tool.

---

## Slash commands are commands, not skills

Skills are not registered as slash commands; the model reaches them through
the `skill` tool. The TUI's user-defined slash commands come from **command**
files instead: `sven_workspace::discover_commands()` scans `commands/`
directories (`.agents/`, `.claude/`, `.codex/`, `.cursor/`, `.sven/`) with the
same ancestor walk as skill discovery, and each `.md` file becomes one command
named after its path relative to the commands root (`sven/plan.md` →
`/sven/plan`). At startup the TUI passes them to
`CommandRegistry::register_commands()` (`sven-commands`), which builds one
`SkillCommand` per file via `make_command_slash_commands()`.

Each `SkillCommand`:
- has `name` = the lowercased command path (e.g. `"sven/plan"`), preserving
  `/`, made unique if two files map to the same name
- reads its `.md` file when executed
- sends the file body - followed by `Task: <args>` when arguments were given -
  as the next agent message

Example:

```
User types:  /sven/plan analyse the authentication module
Message sent:
  <full sven/plan.md body>

  Task: analyse the authentication module
```

---

## Crate responsibilities

| Crate | Responsibility |
|-------|---------------|
| `sven-workspace` | `SkillInfo`, `SvenSkillMeta`, `ParsedSkill`; `parse_skill_file()`; `discover_skills()` and the recursive scanner; requirement checking (`requires_bins`, `requires_env`) |
| `sven-turn` | `build_skills_section()` - serialises skill metadata into the system-prompt XML block; `PromptContext.skills` field |
| `sven-tools-agent` | `SkillTool` - tool implementation, child-detection logic, bundled-file collection |
| `sven-bootstrap` | Calls `discover_skills()`, stores the `Arc<[SkillInfo]>` in `RuntimeContext`, wires it into `AgentRuntimeContext` and registers `SkillTool` |
| `sven-commands` | `SkillCommand`, `make_command_slash_commands()`; `CommandRegistry::register_commands()` for command `.md` files (not skills) |

---

## Domain knowledge convention

Skills and agents covering complex subsystems benefit from embedded domain
knowledge - not just procedural instructions, but project-specific facts,
correctness invariants, and known failure modes.

### Recommended sections for domain-rich skills

When writing a skill (or agent spec) for a complex subsystem, include the
following sections after the procedural instructions:

```markdown
## Domain Knowledge

### Correctness Invariants
- List the properties that must always hold.  Frame as "MUST" / "MUST NOT".
- Example: "All git pathspecs MUST use the `:(glob)` prefix on non-Linux platforms."

### Known Failure Modes
| Symptom | Cause | Fix |
|---------|-------|-----|
| Session hangs after a tool call | No terminal event emitted | Synthesize `ToolFailed` on every error path |
| Approval never resolves | Approval id minted, not derived | Derive the id from the tool call |

### Critical Patterns
- Specific code patterns that must be followed and why.
- Example: "Always derive an approval id from the tool call it guards, never mint a fresh one."
```

### Relationship with the knowledge base

The knowledge base (`.sven/knowledge/`) stores *project-level* specifications
that any agent can retrieve.  Skills and agents can cross-reference them with
an optional `knowledge:` frontmatter field:

```markdown
---
name: hsm-specialist
description: HSM kernel expert. Use when modifying sven-hsm or sven-kernel.
knowledge:
  - sven-hsm.md
  - sven-kernel.md
---

... procedural instructions ...

## Domain Knowledge

... embedded domain facts that are always needed ...
```

**Guideline:** embed the core correctness invariants and most-common failure
modes directly in the skill/agent spec (they are always needed), and link to
the knowledge base for the full architecture narrative and extended failure
tables (loaded on demand).

---

## Token efficiency design

Token efficiency is a first-class concern:

- **Metadata only in system prompt.** The `<available_skills>` block carries
  command, name, and description - never the body.  A typical body is 300-2000
  tokens; keeping only metadata saves the vast majority of that cost.
- **Body loaded on demand.** A skill is loaded at most once per session, and
  only when the model judges it necessary.
- **Sub-skill hint, not body.** When a parent skill is loaded, its children are
  listed as one-liner hints.  The full child body is loaded only if the model
  decides it is needed for the current task.
- **Grandchildren not listed at parent level.** Each level of the hierarchy
  returns only its immediate children, preventing deep trees from bloating any
  single tool call's response.
- **Character budget on system prompt.** The 30 000-character cap prevents a
  large skill library from consuming the model's thinking budget before the
  conversation starts.
