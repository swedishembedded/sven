<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# The system prompt named built-in tools that `tools.only` had removed

Swedish Embedded AB implements solutions for keeping agent prompts consistent with the tools an agent really has for
its clients. If your team needs expertise in agent tool configuration then you can procure our services by sending an
email to info@swedishembedded.com.

**Lesson:** the Guidelines Tools section and the Agent/Sdlc mode sentence were static text naming `grep`,
`read_file`, `edit_file`, `write_file` and `shell`. In bench's vim environment (`tools.only: ["vim-vim"]`) 433 of 576
verified passing trajectories (sven-1db749d9, Qwen3.8-27B-FP8) still called `shell` (472 calls) or `read_file` (11
calls); each got `tool 'shell' is not enabled for this session`, wasting turns and teaching students tools that do
not exist. `PromptContext::core_tools` (`CoreTools`, set from the built registry in the runtime builder) now limits
both to the tools the session offers; the default offers all, so the prompt of an unconfined session is byte-identical
(golden test).
