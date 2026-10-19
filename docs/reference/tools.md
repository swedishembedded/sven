# Tools and approvals

## Tools and approvals

sven has a set of built-in tools it can call to complete tasks:

| Tool | What it does |
|------|-------------|
| `read_file` | Read a file (images are detected and attached) |
| `write_file` | Create or overwrite a file |
| `edit_file` | Edit part of a file |
| `attach_file` | Put an image, audio file or PDF into the model's context |
| `find_file` | Find files by name pattern |
| `grep` | Search file contents, in one file or the whole project |
| `shell` | Run a shell command (also how files are deleted and directories listed) |
| `web_fetch` | Fetch a URL |
| `web_search` | Search the web |
| `task` | Delegate a self-contained subtask to a sub-agent |
| `context` | Work with content larger than the context window |
| `memory` | Key-value notes and project knowledge documents |
| `semantic_memory` | Remember and recall facts across sessions |
| `todo` | Read or update the task list for the current session (call with no args to read) |
| `skill` | Load a skill's instructions |
| `system` | Change the agent mode or model mid-session |
| `ask_question` | Ask you a clarifying question (interactive sessions) |
| `gdb` | Drive a GDB debugging session (offered when the project has GDB configuration) |

### Approval policy

By default (`--approval auto`) every call the mode allows runs without a
prompt. A shell command is also checked against two lists of patterns:

- **Denied** patterns (`tools.deny_patterns`, e.g. `rm -rf /*`) are refused
  outright, in every approval mode.
- **Auto-approved** patterns (`tools.auto_approve_patterns`, e.g. `cat *`,
  `ls *`, `grep *`) run without a prompt under `--approval manual`; every
  other call that is not read-only is put to you first.

You can customise these patterns in the configuration file - see
[Configuration](configuration.md).
