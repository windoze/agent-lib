use super::{AgentWorkerBuilder, INHERITED_MODEL_PLACEHOLDER, LocalSubagent};
use crate::facade::approval::Approval;
use crate::facade::error::FacadeError;
use crate::facade::ids::FacadeIds;
use crate::model::extras::{ProviderExtras, ProviderId};
use crate::model::tool::Tool as ToolDecl;
use serde_json::{Map, json};

fn worker() -> AgentWorkerBuilder {
    AgentWorkerBuilder::default()
}

fn review_decl() -> ToolDecl {
    ToolDecl {
        name: "grep".to_owned(),
        description: "Search the tree.".to_owned(),
        input_schema: json!({ "type": "object" }),
    }
}

fn provider_extras() -> ProviderExtras {
    ProviderExtras {
        provider: ProviderId::Anthropic,
        fields: Map::from_iter([("top_k".to_owned(), json!(25))]),
    }
}

#[test]
fn explicit_model_worker_is_data_only_and_not_inheriting() {
    let extras = provider_extras();
    let sub = worker()
        .description("Strict reviewer")
        .model("gpt-5.5")
        .temperature(0.1)
        .provider_extras(extras.clone())
        .system("You review code.")
        .build()
        .expect("worker builds");

    assert_eq!(sub.name(), "");
    assert_eq!(sub.description(), "Strict reviewer");
    assert!(!sub.inherits_model());
    assert_eq!(sub.spec().model().model(), "gpt-5.5");
    assert_eq!(sub.spec().model().temperature(), Some(0.1));
    assert_eq!(sub.spec().model().provider_extras(), Some(&extras));
    assert_eq!(sub.spec().system_prompt(), Some("You review code."));
    assert!(sub.tools().tools().is_empty());

    // Data-first: the spec round-trips through serde with no runtime handles.
    let value = serde_json::to_value(sub.spec()).expect("spec serializes");
    assert_eq!(value["model"]["model"], "gpt-5.5");
}

#[test]
fn worker_inherits_model_by_default() {
    let sub = worker().system("reviewer").build().expect("worker builds");

    assert!(sub.inherits_model());
    assert_eq!(sub.spec().model().model(), INHERITED_MODEL_PLACEHOLDER);
}

#[test]
fn inherit_and_explicit_toggle_last_call_wins() {
    // model(..) after inherit_model() pins the model.
    let pinned = worker()
        .inherit_model()
        .model("gpt-5.5")
        .build()
        .expect("worker builds");
    assert!(!pinned.inherits_model());
    assert_eq!(pinned.spec().model().model(), "gpt-5.5");

    // inherit_model() after model(..) reverts to inheritance.
    let inherited = worker()
        .model("gpt-5.5")
        .inherit_model()
        .build()
        .expect("worker builds");
    assert!(inherited.inherits_model());
    assert_eq!(
        inherited.spec().model().model(),
        INHERITED_MODEL_PLACEHOLDER
    );
}

#[test]
fn inherited_worker_rejects_provider_extras_without_explicit_model() {
    let error = worker()
        .provider_extras(provider_extras())
        .build()
        .expect_err("inherited model has no worker-local provider extras slot");

    let FacadeError::Config(message) = error else {
        panic!("expected config error")
    };
    assert!(message.contains("provider_extras"));
}

#[test]
fn explicit_worker_rejects_blank_model() {
    let error = worker()
        .model("  ")
        .build()
        .expect_err("blank model is rejected");

    let FacadeError::Config(message) = error else {
        panic!("expected config error")
    };
    assert!(message.contains("model"));
}

#[test]
fn explicit_worker_rejects_non_finite_temperature() {
    let error = worker()
        .model("gpt-5.5")
        .temperature(f32::NEG_INFINITY)
        .build()
        .expect_err("non-finite temperature is rejected");

    let FacadeError::Config(message) = error else {
        panic!("expected config error")
    };
    assert!(message.contains("temperature"));
}

#[test]
fn tool_declarations_flow_into_the_child_spec() {
    let sub = worker()
        .tool_declarations(vec![review_decl()])
        .build()
        .expect("worker builds");

    assert_eq!(sub.tools().tools().len(), 1);
    assert_eq!(sub.tools().tools()[0].name, "grep");
    // The spec's initial tool set mirrors the exposed declarations.
    assert_eq!(
        sub.spec().initial_tools().tools()[0].name,
        sub.tools().tools()[0].name
    );
}

#[test]
fn approval_policy_is_carried_through() {
    let sub = worker()
        .approval(Approval::auto_deny())
        .build()
        .expect("worker builds");
    // A defaulted policy is present and usable (data, no handler required).
    let _ = sub.approval();
}

#[test]
fn deterministic_ids_yield_stable_spec_identity() {
    let a = worker()
        .ids(FacadeIds::seeded(100))
        .build()
        .expect("worker builds");
    let b = worker()
        .ids(FacadeIds::seeded(100))
        .build()
        .expect("worker builds");
    assert_eq!(a.spec().id(), b.spec().id());
}

#[test]
fn with_name_stamps_the_registration_name() {
    let sub = worker()
        .build()
        .expect("worker builds")
        .with_name("reviewer");
    assert_eq!(sub.name(), "reviewer");
    // Cloning a LocalSubagent keeps it data-only and equal by field.
    let clone: LocalSubagent = sub.clone();
    assert_eq!(clone.name(), "reviewer");
}

#[test]
fn delegation_declaration_advertises_ask_tool_with_task_input() {
    use super::{delegation_declaration, delegation_tool_name};

    assert_eq!(delegation_tool_name("reviewer"), "ask_reviewer");

    let decl = delegation_declaration("reviewer", "Strict code reviewer.");
    assert_eq!(decl.name, "ask_reviewer");
    assert_eq!(decl.description, "Strict code reviewer.");
    assert_eq!(decl.input_schema["properties"]["task"]["type"], "string");
    assert_eq!(decl.input_schema["required"][0], "task");

    // A blank description gets a terse generated one so no tool is advertised
    // without any hint of its purpose.
    let generated = delegation_declaration("researcher", "");
    assert_eq!(
        generated.description,
        "Delegate a task to the `researcher` subagent."
    );
}
