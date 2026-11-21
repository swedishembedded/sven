<!-- SPDX-License-Identifier: CC-BY-4.0 -->
<!-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com> -->

# `model.driver_options` next to `model.provider: <entry>` was silently dropped

Swedish Embedded AB implements solutions for reproducible agent request settings for its clients. If your team needs
expertise in agent model configuration then you can procure our services by sending an email to
info@swedishembedded.com.

**Lesson:** `config_settle::expand_named_provider` replaced the whole `model:` section with the named provider
entry's settings, so a `driver_options` (for example `thinking_budget`) written in `model:` was discarded without a
warning. A bench run with `[sven.driver_options] thinking_budget = 16` rendered it into `model:` and the budget never
reached the upstream (reasoning stayed 3,000-4,500 characters per turn in sven-1db749d9). Direct requests with the
field in the body did cap reasoning, which located the loss in sven's config settle, not in the gateway.

**Fix:** the options in `model:` are laid over the entry's `driver_options` key by key (model wins). Binaries built
before the fix need the options under `providers.<entry>.models.<model>.driver_options` (or the entry's
`driver_options`), which `to_model_config` honours.

## Every other `model:` field was dropped the same way (2026-11-21)

`enable_thinking: false` next to `provider: gateway` reached the rendered config of a bench trial, but the recorded
requests carried no `chat_template_kwargs` and the 4B reasoned 3,600-8,800 characters per turn (sven-1db749d9, two vim
trials). The expansion replaced the whole section, so only `driver_options` (fixed above) survived. Now every field the
`model:` section sets (differs from `ModelConfig::default`) lies over the entry's, a field left at its default keeps
the entry's, and the process-only `session` is carried over (`config_settle::overlay_requested`; spec
`every_setting_a_model_section_gives_survives_the_expansion_of_its_named_provider`). Until a build with this is used,
put the switch in `driver_options.chat_template_kwargs.enable_thinking`, which every build honours.
