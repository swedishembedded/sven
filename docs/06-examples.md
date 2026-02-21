# Examples

This section walks through real-world scenarios with complete commands and
explanations. Each example can be adapted to your own project.

---

## Example 1 — Understand an unfamiliar codebase

You have just joined a project and want to get up to speed quickly. Use
`research` mode so sven cannot make any accidental changes.

```sh
cd my-project
sven --mode research "Give me a tour of this codebase. Explain the purpose of each top-level directory and identify the main entry points."
```

For a deeper dive, use a conversation file so you can ask follow-up questions:

```sh
cat > onboarding.md << 'EOF'
# Codebase onboarding

## User
Give me an overview of this project: its purpose, the main modules, and the technology stack.

## User
Which parts of the code have the most activity (most recently changed files)?

## User
Where is the authentication logic? Show me the key files and summarise how it works.
EOF

sven --file onboarding.md
```

---

## Example 2 — Generate and review an implementation plan

Use `plan` mode to get a written design before any code is written. Review the
plan, then hand it to an `agent` run to implement it.

**Step 1 — produce the plan:**

```sh
sven --mode plan "Design a rate-limiting middleware for our Express API. The limit should be configurable per endpoint. Use Redis for storage." \
  > rate-limiter-plan.txt

cat rate-limiter-plan.txt
```

**Step 2 — implement the plan:**

```sh
cat rate-limiter-plan.txt | sven --mode agent
```

Or run both steps as a pipeline:

```sh
echo "Design a rate-limiting middleware for our Express API with configurable per-endpoint limits and Redis storage." \
  | sven --mode plan \
  | sven --mode agent
```

---

## Example 3 — Targeted code refactoring

Ask sven to refactor a specific file while keeping the API intact.

```sh
sven "Refactor src/services/user.ts. The file is too large. Split it into separate files for authentication, profile management, and session handling. Keep all existing exports working."
```

To see what changes sven would make before it makes them:

```sh
sven --mode plan "Describe the refactoring steps for src/services/user.ts — splitting auth, profile, and session into separate files."
```

---

## Example 4 — Write tests for existing code

```sh
sven "Write unit tests for the functions in src/utils/validation.js. Use the Jest framework. Aim for at least 80% coverage of the exported functions. Place the tests in tests/utils/validation.test.js."
```

If there are existing tests you want sven to follow:

```sh
sven "Look at the test style in tests/utils/string.test.js and write similar tests for src/utils/validation.js."
```

---

## Example 5 — Review a diff and flag issues

A common CI task: take a diff and ask sven to find bugs or style issues.

```sh
git diff main...HEAD | sven --mode research "Review this diff. Flag any bugs, security issues, or code style problems. Be specific about file names and line numbers."
```

In a CI workflow:

```yaml
# GitHub Actions example
- name: sven code review
  env:
    OPENAI_API_KEY: ${{ secrets.OPENAI_API_KEY }}
  run: |
    git diff origin/main...HEAD \
      | sven --mode research \
              "Review this pull request diff. List bugs, security issues, and style problems."
```

---

## Example 6 — Multi-stage CI pipeline

This example shows a three-stage pipeline: research → plan → implement.

```sh
# Stage 1: identify technical debt (research only, fast)
echo "List the top five sources of technical debt in this project with a brief explanation of each." \
  | sven --mode research \
  > debt-report.txt

# Stage 2: turn the report into a prioritised action plan
cat debt-report.txt \
  | sven --mode plan \
  > action-plan.txt

# Stage 3: implement the first item in the plan
echo "Implement the first item from the following plan:" \
  | cat - action-plan.txt \
  | sven --mode agent
```

---

## Example 7 — Resuming a long conversation

For tasks that span multiple sessions (e.g. a large feature implementation),
use `--resume` to continue where you left off.

```sh
# Start the work
sven "We are going to implement a new reporting module. Let's start by mapping out all the data sources we will need."

# The session ID is shown when sven exits, or list it with:
sven chats

# Resume the next day
sven --resume 3f4a   # use the ID shown by 'sven chats'
```

Or pick interactively (requires `fzf`):

```sh
sven --resume
```

---

## Example 8 — Using the mock provider for testing

The mock provider lets you test scripts and CI jobs without making real API
calls. It matches input messages against rules in a YAML file.

**Create a mock responses file:**

```yaml
# mock-responses.yaml
rules:
  - match: "ping"
    match_type: equals
    reply: "pong"

  - match: "summarise"
    match_type: substring
    reply: "This is a mock summary."

  - match: ".*"
    match_type: regex
    reply: "I am the mock agent. I received your message."
```

**Test a pipeline script with the mock provider:**

```sh
export SVEN_MOCK_RESPONSES="$PWD/mock-responses.yaml"

echo "ping" | sven --model mock --headless
# Output: pong

echo "Please summarise the project." | sven --model mock --headless
# Output: This is a mock summary.
```

**Use the mock in CI to validate pipeline structure without API cost:**

```yaml
- name: Validate pipeline script
  env:
    SVEN_MOCK_RESPONSES: tests/fixtures/mock-responses.yaml
  run: sven --model mock --file ci/validate.md
```

---

## Example 9 — Conversation files for iterative development

This pattern works well for ongoing tasks where you want a readable record.

```sh
# Create the file
cat > feature.md << 'EOF'
# Shopping cart feature

## User
Analyse the existing product and user modules to understand what data is available.
EOF

# First pass — sven reads, analyses, and appends its response
sven --file feature.md --conversation

# Review the response in your editor, then add the next step
printf '\n## User\nBased on what you found, implement a basic shopping cart model in src/models/cart.ts.\n' \
  >> feature.md

# Second pass — sven sees the full history and implements
sven --file feature.md --conversation

# Continue iterating...
printf '\n## User\nWrite tests for the cart model.\n' >> feature.md
sven --file feature.md --conversation
```

The file accumulates a complete, readable history of the work.

---

## Example 10 — Web research and code generation

Ask sven to look something up and then write code based on what it finds.

```sh
sven "Fetch the latest stable version number from https://github.com/example/lib/releases and update the version in our go.mod file to match it."
```

Or for documentation lookups:

```sh
sven "Fetch the API documentation from https://api.example.com/docs and generate a typed TypeScript client for the /users and /orders endpoints."
```

---

## Example 11 — Embedded GDB debugging session

Flash firmware and step through an embedded target using sven's integrated GDB
tools. The agent discovers the device automatically from project files, or you
can tell it the device explicitly.

```sh
sven "Flash the firmware to the AT32F435RMT7 and check the value of the
SystemCoreClock variable after the clock initialisation."
```

sven will:

1. Start JLinkGDBServer with the correct `-device` and `-port` flags
2. Spawn `gdb-multiarch --interpreter=mi3` and connect with `target remote :2331`
3. Load the ELF binary (`load`)
4. Set a breakpoint past clock init and `continue`
5. Print `SystemCoreClock` with `print SystemCoreClock`
6. Call `gdb_stop` to kill the server and free the debug probe

You can also drive the debug session step by step in a conversation file:

```markdown
# Firmware Debug

## User
Start the GDB server. The device is STM32F407VG, SWD, 4000 kHz, port 2331.

## User
Connect gdb-multiarch to it and load build/firmware.elf.

## User
Set a breakpoint at HAL_Init and continue. Show me the backtrace when it hits.

## User
Read the GPIOA ODR register (address 0x40020014) as a 32-bit hex value.

## User
Stop the debugging session.
```

```sh
sven --file firmware-debug.md --conversation
```
