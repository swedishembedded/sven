// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The terms a parent held to an allow list hands its sub-agent
//! ([`ParentTerms`]) and the endpoints they carry ([`TermsEndpoint`]).
//!
//! Swedish Embedded AB implements solutions for confining delegated agents to
//! the authority of their parent for its clients. If your team needs expertise
//! in least-privilege delegation for agent runtimes, you can procure our
//! services by sending an email to info@swedishembedded.com.

/// What a parent held to an allow list (`tools.only`: a hosted session)
/// hands its sub-agent before anything else, as one JSON line on its stdin
/// (`sven acp serve --parent-terms-stdin`): the same allow list without
/// `task`, the same web and location limits, no MCP server of its own, and
/// the endpoints and key the parent itself works with. A child is never
/// given more than its parent has, and the key never reaches a file or an
/// environment variable.
#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ParentTerms {
    /// The allow list of a held parent; `None` for a parent that is confined
    /// but not held to one, whose child keeps its own tool set.
    #[serde(default)]
    pub only_tools: Option<Vec<String>>,
    pub web_public_only: bool,
    pub hide_locations: bool,
    pub system_prompt: Option<String>,
    /// The parent's model endpoint (an OpenAI-style base URL and its key).
    pub model: Option<TermsEndpoint>,
    /// The provider the parent names its models under (`openai/<model>`),
    /// which a model the child is told to use is named without.
    pub model_provider: String,
    /// Where the parent runs other models, and the conversation it names.
    pub media: Option<TermsEndpoint>,
    pub media_session: Option<String>,
    /// The service the parent searches the web through.
    pub search: Option<TermsEndpoint>,
    /// The confinement of the commands the child's tools start, set per child
    /// by the parent that confines its own: the same tree and nothing more
    /// (no network at all, unlike the child process itself, which must reach
    /// its model).
    #[serde(default)]
    pub confine: Option<TermsConfinement>,
}

/// The directories a confined parent lets its commands use, handed to a
/// child so the commands of the child's tools are held to the same.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TermsConfinement {
    pub writable: Vec<std::path::PathBuf>,
    pub read_exec: Vec<std::path::PathBuf>,
}

/// An endpoint and its key, as [`ParentTerms`] carries one.
#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TermsEndpoint {
    pub base_url: String,
    pub api_key: String,
}

impl std::fmt::Debug for TermsEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TermsEndpoint")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for ParentTerms {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParentTerms")
            .field("only_tools", &self.only_tools)
            .finish_non_exhaustive()
    }
}

impl ParentTerms {
    /// The terms `config` (a session's own) holds its sub-agents to, when it
    /// is held to an allow list; none otherwise, and the child reads its own.
    #[must_use]
    pub fn of(config: &crate::Config) -> Option<Self> {
        // A confined parent hands its child the model endpoint too: the
        // child's process is confined and reads no configuration of its own.
        if config.tools.only.is_none() && config.tools.confine.is_none() {
            return None;
        }
        let only = config.tools.only.as_ref();
        let endpoint =
            |base_url: &Option<String>, api_key: &Option<String>| match (base_url, api_key) {
                (Some(base_url), Some(api_key)) => Some(TermsEndpoint {
                    base_url: base_url.clone(),
                    api_key: api_key.clone(),
                }),
                _ => None,
            };
        let media = config.media_endpoint();
        Some(Self {
            only_tools: only.map(|only| {
                only.iter()
                    .filter(|name| name.as_str() != "task")
                    .cloned()
                    .collect()
            }),
            web_public_only: config.tools.web.public_only,
            hide_locations: config.tools.hide_locations,
            system_prompt: config.agent.system_prompt.clone(),
            model: endpoint(&config.model.base_url, &config.model.api_key),
            model_provider: config.model.provider.clone(),
            media: media.as_ref().map(|(e, _)| TermsEndpoint {
                base_url: e.base_url.clone(),
                api_key: e.api_key.clone(),
            }),
            media_session: media.and_then(|(_, c)| c.session),
            search: config
                .tools
                .web
                .search
                .sven
                .as_ref()
                .map(|e| TermsEndpoint {
                    base_url: e.base_url.clone(),
                    api_key: e.api_key.clone(),
                }),
            confine: None,
        })
    }

    /// The name `model` (as a parent names it, perhaps `provider/model`) has
    /// at the parent's endpoint.
    #[must_use]
    pub fn model_name<'a>(&self, model: &'a str) -> &'a str {
        model
            .strip_prefix(&format!("{}/", self.model_provider))
            .unwrap_or(model)
    }

    /// `config` (the child's) held to these terms: each only narrows what it
    /// would have, and the endpoints are the parent's.
    pub fn apply(&self, config: &mut crate::Config) {
        if let Some(only) = &self.only_tools {
            config.tools.only = Some(
                only.iter()
                    .filter(|name| name.as_str() != "task")
                    .cloned()
                    .collect(),
            );
        }
        config.mcp_servers.clear();
        // The commands of this process's tools are confined like the parent's
        // (a nested domain: it can only narrow the process's own).
        if let Some(confine) = &self.confine {
            config.tools.confine = Some(sven_tools_exec::confine::CommandConfinement {
                writable: confine.writable.clone(),
                read_exec: confine.read_exec.clone(),
                tmpdir: sven_roots::state_root().map(|root| root.join("tmp")),
                connect_ports: Vec::new(),
            });
        }
        config.tools.web.public_only |= self.web_public_only;
        config.tools.hide_locations |= self.hide_locations;
        if self.system_prompt.is_some() {
            config.agent.system_prompt.clone_from(&self.system_prompt);
        }
        if let Some(model) = &self.model {
            config.model.provider = "openai".into();
            config.model.base_url = Some(model.base_url.clone());
            config.model.api_key = Some(model.api_key.clone());
            // Its model calls are the parent's conversation's, as its media
            // calls are.
            config.model.session.clone_from(&self.media_session);
        }
        if let Some(media) = &self.media {
            config.tools.media.provider = None;
            config.tools.media.service = Some(sven_tools_media::MediaEndpoint {
                base_url: media.base_url.clone(),
                api_key: media.api_key.clone(),
            });
        }
        config.tools.media.session.clone_from(&self.media_session);
        if let Some(search) = &self.search {
            config.tools.web.search.sven = Some(sven_tools_web::SvenEndpoint {
                base_url: search.base_url.clone(),
                api_key: search.api_key.clone(),
            });
        }
    }
}
