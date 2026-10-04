//! One verb's answer, in both the shape an agent reads and the shape a
//! person does.
//!
//! The `--json` contract in docs/agents.md is a published interface:
//! mutating verbs emit exactly one document on stdout — `ok`, their own
//! fields, and `actions` last — or the one failure document, which main
//! already prints centrally. Before this module that contract was
//! maintained by discipline at dozens of hand-built `json!` sites, and
//! the suite policed the verb lists by grepping main.rs's own source
//! text, which is what a missing interface looks like.
//!
//! Here the document is assembled once and rendered once. A verb sets
//! its fields, marks a dry run, attaches its actions, and prints;
//! `ok` and `actions` cannot be forgotten because they are not typed
//! anywhere, and a field rename touches the one place the field is set.

use crate::state::{action_json, print_actions, Action};
use serde_json::Value;

pub struct Response {
    /// Insertion-ordered, because the verb knows which of its fields is
    /// the headline and a reader — human or agent — should meet it
    /// first.
    fields: Vec<(&'static str, Value)>,
    /// Marked rather than passed: a dry run that forgets to say so is a
    /// document an agent cannot distinguish from the real thing.
    dry_run: bool,
    actions: Vec<Action>,
}

impl Response {
    pub fn new() -> Response {
        Response { fields: Vec::new(), dry_run: false, actions: Vec::new() }
    }

    /// One field of the verb's own. Values are the JSON shapes the
    /// contract already uses; nothing here invents new ones.
    ///
    /// The contract keys are the document's, inserted by `document()`
    /// after the fields land in the same map — so a field named like
    /// one of them would silently clobber `ok`, `dry_run` or `actions`
    /// and break the one promise every agent reads first. No current
    /// call site does; the assertion is what keeps it that way when a
    /// future one is written in a hurry.
    pub fn field(mut self, key: &'static str, value: impl Into<Value>) -> Response {
        debug_assert!(
            !matches!(key, "ok" | "dry_run" | "actions"),
            "{key} is a contract key the document owns, not a verb field"
        );
        self.fields.push((key, value.into()));
        self
    }

    /// Label this document a dry run. Nothing has been changed, and an
    /// agent polling for effect must be able to tell that from the
    /// document alone.
    pub fn dry_run(mut self) -> Response {
        self.dry_run = true;
        self
    }

    /// One affordance the reader may act on.
    pub fn action(mut self, action: Action) -> Response {
        self.actions.push(action);
        self
    }

    /// All of them at once, for the verbs that gathered their actions
    /// before deciding which document to print.
    pub fn actions(mut self, actions: &[Action]) -> Response {
        self.actions.extend(actions.iter().cloned());
        self
    }

    /// Print the answer: one JSON document, or the prose with the
    /// actions after it. This is the only exit a response has, so the
    /// one-document promise is not a convention a verb can fumble.
    ///
    /// The prose is the verb's own: what changed, what would change,
    /// and whatever a person needs that no field carries. It is not
    /// assembled from the fields, because the two audiences genuinely
    /// want different things said.
    pub fn print(self, json: bool, prose: &str) {
        if json {
            println!("{}", self.document());
        } else {
            println!("{prose}");
            print_actions(&self.actions);
        }
    }

    /// The document itself. `pub(crate)` so the shape tests can hold a
    /// verb's exact keys without running the verb: the extracted
    /// builders return a `Response`, and the tests read the document it
    /// would print.
    ///
    /// `ok` first because it is the field every consumer reads
    /// first, `actions` last because it is the one every consumer
    /// follows.
    pub(crate) fn document(&self) -> Value {
        let mut map = serde_json::Map::new();
        map.insert("ok".to_string(), Value::Bool(true));
        if self.dry_run {
            map.insert("dry_run".to_string(), Value::Bool(true));
        }
        for (key, value) in &self.fields {
            map.insert(key.to_string(), value.clone());
        }
        map.insert(
            "actions".to_string(),
            Value::Array(self.actions.iter().map(action_json).collect()),
        );
        Value::Object(map)
    }
}

/// The marker a `--json` verb returns when its own document has already
/// ended stdout machine-readably with the failure carried inside it.
/// Main's central failure document exists for verbs whose failure left
/// stdout silent; appending one to a document that exists makes stdout
/// two documents back to back, and two documents is the one shape no
/// caller can parse. The summary still rides stderr through main's
/// Result, so scripts keep both signals: one document on stdout, the
/// verdict again on stderr, and a non-zero exit.
#[derive(Debug)]
pub struct Emitted(pub String);

impl std::fmt::Display for Emitted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Emitted {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The contract, pinned: `ok` is there, `dry_run` is there exactly
    /// when the verb marked it, and `actions` rides last — the shape
    /// docs/agents.md promises and 32 hand-built documents had to be
    /// trusted to keep.
    #[test]
    fn the_document_carries_the_contract() {
        let doc = Response::new()
            .field("tag", "localhost/kuma:latest")
            .field("image_built", true)
            .action(Action::new("apply", "kuma switch --yes", "stage it"))
            .document();
        assert_eq!(doc.get("ok"), Some(&Value::Bool(true)));
        assert_eq!(doc.get("tag"), Some(&"localhost/kuma:latest".into()));
        assert_eq!(doc.get("image_built"), Some(&Value::Bool(true)));
        assert!(doc.get("dry_run").is_none());
        let actions = doc.get("actions").expect("actions ride every document").as_array().unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].get("rel"), Some(&"apply".into()));

        let dry = Response::new().dry_run().document();
        assert_eq!(dry.get("dry_run"), Some(&Value::Bool(true)));
        assert_eq!(dry.get("ok"), Some(&Value::Bool(true)));
    }

    /// A verb that forgot its fields still answers honestly — ok, no
    /// dry_run, empty actions — rather than an absent document or a
    /// malformed one. The floor is the contract.
    #[test]
    fn an_empty_response_is_still_the_contract() {
        let doc = Response::new().document();
        assert_eq!(doc.get("ok"), Some(&Value::Bool(true)));
        assert!(doc.get("dry_run").is_none());
        assert_eq!(doc.get("actions"), Some(&Value::Array(vec![])));
    }

    /// The clobber the assertion in `field` exists for: a field with a
    /// contract key would ride the same map in after the real one and
    /// replace it. Debug-only, like the assertion — a release build has
    /// no runtime cost, and a debug one fails here instead of shipping
    /// a document that lies about `ok`.
    #[test]
    #[should_panic(expected = "contract key")]
    fn a_field_cannot_take_a_contract_key() {
        let _ = Response::new().field("ok", false);
    }
}
