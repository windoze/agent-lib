use super::fixtures::{conversation, message, text, text_draft, turn_id};
use crate::{
    conversation::{CommitError, ConversationError, TurnMeta, turn::TurnData},
    model::message::Role,
};

#[test]
fn rejected_commit_leaves_history_version_identity_and_config_unchanged() {
    let mut conversation = conversation();
    let first_id = conversation
        .commit_draft(text_draft(10, None, 100))
        .expect("commit baseline turn");
    let before = conversation.clone();
    let invalid = TurnData {
        id: turn_id(11),
        messages: vec![
            message(200, Role::User, vec![text("question")]),
            message(201, Role::Assistant, vec![text("answer")]),
        ],
        pairings: Vec::new(),
        parent: None,
        meta: TurnMeta::default(),
        completion: crate::conversation::turn::TurnCompletion::Complete,
    };

    let error = conversation
        .commit_draft(invalid)
        .expect_err("wrong parent must fail atomically");

    assert_eq!(
        error,
        ConversationError::Commit(CommitError::ParentMismatch {
            expected: Some(first_id),
            actual: None,
        })
    );
    assert_eq!(conversation, before);
}

#[test]
fn a_validation_failure_does_not_poison_the_next_valid_commit() {
    let mut conversation = conversation();
    let invalid = text_draft(10, Some(turn_id(999)), 100);
    let before = conversation.clone();

    assert!(conversation.commit_draft(invalid).is_err());
    assert_eq!(conversation, before);

    let committed = conversation
        .commit_draft(text_draft(10, None, 100))
        .expect("retry valid candidate");
    assert_eq!(committed, turn_id(10));
    assert_eq!(conversation.version(), 1);
    assert_eq!(conversation.turns().len(), 1);
}

#[test]
fn version_exhaustion_is_classified_before_history_can_change() {
    let mut conversation = conversation();
    conversation.version = u64::MAX;
    let before = conversation.clone();

    let error = conversation
        .commit_draft(text_draft(10, None, 100))
        .expect_err("version overflow must reject the transaction");

    assert_eq!(
        error,
        ConversationError::NonAtomicCommit {
            current_version: u64::MAX,
        }
    );
    assert_eq!(conversation, before);
}
