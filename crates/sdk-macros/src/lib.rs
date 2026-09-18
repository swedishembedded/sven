// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `#[agent]` attribute: declare an agent as an ordinary Rust trait.
//!
//! The whole purpose is that there is nowhere for a prompt to drift to. The
//! role is the trait's documentation, each task is its method's documentation,
//! and each schema is derived from the method's return type - so changing the
//! contract and changing what the model is told are a single edit.
//!
//! Re-exported as `sven_sdk::agent`; use it from there.
//!
//! Swedish Embedded AB implements typed agent interfaces for its clients. If
//! your team needs expertise in keeping model instructions and application
//! types from drifting apart then you can procure our services by sending an
//! email to info@swedishembedded.com.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{
    parse_macro_input, Attribute, Expr, ExprLit, FnArg, ItemTrait, Lit, Meta, Pat, ReturnType,
    TraitItem,
};

/// Declares an agent from a trait.
///
/// A method **without a body** is model-driven: it becomes an `async fn`
/// returning `Result<T, CallError>`, where `T` is the declared return type.
/// A method **with a body** is deterministic and is emitted unchanged - which
/// is how a policy check or an arithmetic rule stays out of the model's hands.
///
/// ```ignore
/// /// You are a meticulous Rust reviewer who never speculates.
/// #[sven_sdk::agent]
/// trait Reviewer {
///     /// Assess the change for correctness risk.
///     async fn assess(&self, change: Change) -> Assessment;
///
///     /// Deterministic: no model involved.
///     fn may_merge(&self, a: &Assessment) -> bool {
///         a.risk < 50
///     }
/// }
/// ```
///
/// The generated type owns an agent, so it suspends and resumes like any
/// other: `new`, `resume`, `suspend`, `events`, `state`.
#[proc_macro_attribute]
pub fn agent(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemTrait);
    match expand(&input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Builds the concrete agent type from the trait declaration.
fn expand(input: &ItemTrait) -> syn::Result<proc_macro2::TokenStream> {
    let name = &input.ident;
    let vis = &input.vis;
    let role = doc_of(&input.attrs);
    if role.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "an #[agent] trait needs a doc comment: it is the agent's role, and \
             without one the model is told nothing about who it is",
        ));
    }

    let mut methods = Vec::new();
    for item in &input.items {
        let TraitItem::Fn(method) = item else {
            return Err(syn::Error::new_spanned(
                item,
                "an #[agent] trait may only contain methods",
            ));
        };

        // A body means the author already said what should happen, so it is not
        // the model's business. This is the whole deterministic/model-driven
        // split, expressed as something the compiler already tracks.
        if let Some(body) = &method.default {
            let sig = &method.sig;
            let attrs = &method.attrs;
            methods.push(quote! { #(#attrs)* #vis #sig #body });
            continue;
        }

        methods.push(model_driven(method)?);
    }

    let doc = format!(
        "Agent: {}",
        role.lines().next().unwrap_or(name.to_string().as_str())
    );
    Ok(quote! {
        #[doc = #doc]
        #vis struct #name {
            agent: ::sven_sdk::Agent,
        }

        impl #name {
            /// The role this agent plays, taken from the trait's documentation.
            #vis const ROLE: &'static str = #role;

            /// Creates a fresh agent against `engine`.
            #[must_use]
            #vis fn new(engine: &::sven_sdk::Engine) -> Self {
                Self {
                    agent: engine.agent_with_role("predict", Self::ROLE),
                }
            }

            /// Resumes a suspended agent against `engine`.
            ///
            /// # Errors
            ///
            /// Returns an error if `engine` cannot run the state's mode.
            #vis fn resume(
                engine: &::sven_sdk::Engine,
                state: ::sven_sdk::AgentState,
            ) -> ::std::result::Result<Self, ::sven_sdk::CallError> {
                Ok(Self { agent: engine.resume(state)? })
            }

            /// Suspends the agent, yielding the state needed to resume it.
            #[must_use]
            #vis fn suspend(self) -> ::sven_sdk::AgentState {
                self.agent.suspend()
            }

            /// The agent's current state, including its history.
            #[must_use]
            #vis fn state(&self) -> &::sven_sdk::AgentState {
                self.agent.state()
            }

            /// Subscribes to everything this agent emits while it works.
            #[must_use]
            #vis fn events(
                &self,
            ) -> ::tokio::sync::broadcast::Receiver<::sven_sdk::SessionEvent> {
                self.agent.events()
            }

            #(#methods)*
        }
    })
}

/// Expands one bodyless method into a typed model-driven call.
fn model_driven(method: &syn::TraitItemFn) -> syn::Result<proc_macro2::TokenStream> {
    let sig = &method.sig;
    let name = &sig.ident;
    let name_str = name.to_string();
    let task = doc_of(&method.attrs);
    if task.is_empty() {
        return Err(syn::Error::new_spanned(
            &sig.ident,
            "a model-driven method needs a doc comment: it is the task the \
             model is given, and without one it is told only the method's name",
        ));
    }

    let ReturnType::Type(_, output) = &sig.output else {
        return Err(syn::Error::new_spanned(
            sig,
            "a model-driven method must declare what it returns; the return \
             type is what the model's answer is validated against",
        ));
    };

    // Every parameter after the receiver becomes a named field of the input
    // object, so the model sees `{"change": {...}}` rather than a positional
    // blob it has to guess the meaning of.
    let mut keys = Vec::new();
    let mut values = Vec::new();
    for arg in &sig.inputs {
        let FnArg::Typed(typed) = arg else { continue };
        let Pat::Ident(ident) = &*typed.pat else {
            return Err(syn::Error::new_spanned(
                &typed.pat,
                "a model-driven method's parameters must be plain names: each \
                 one is given to the model under that name",
            ));
        };
        keys.push(ident.ident.to_string());
        values.push(ident.ident.clone());
    }

    let params = sig.inputs.iter().filter(|a| matches!(a, FnArg::Typed(_)));
    let method_ident = format_ident!("__sven_method_{}", name);
    let docs = &method.attrs;

    Ok(quote! {
        #(#docs)*
        ///
        /// Model-driven: the body is obtained from the model, validated against
        /// the declared return type, and returned or reported as a failure.
        pub async fn #name(
            &mut self,
            #(#params),*
        ) -> ::std::result::Result<#output, ::sven_sdk::CallError> {
            let #method_ident = ::sven_sdk::Method::<#output>::new(#name_str)
                .role(Self::ROLE)
                .task(#task);
            let input = ::serde_json::json!({
                #(#keys: #values),*
            });
            self.agent.call(&#method_ident, &input).await
        }
    })
}

/// Joins a run of `///` lines into one block of text.
fn doc_of(attrs: &[Attribute]) -> String {
    let mut lines = Vec::new();
    for attr in attrs {
        let Meta::NameValue(nv) = &attr.meta else {
            continue;
        };
        if !nv.path.is_ident("doc") {
            continue;
        }
        if let Expr::Lit(ExprLit {
            lit: Lit::Str(text),
            ..
        }) = &nv.value
        {
            lines.push(text.value().trim().to_string());
        }
    }
    lines.join("\n").trim().to_string()
}
