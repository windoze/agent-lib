use super::*;

pub(crate) fn assert_state_machine_invariants(label: &str, conversation: &Conversation) {
    assert_eq!(
        conversation.head().turn_count(),
        conversation.turns().len() as u64,
        "{label}: head must clip exactly the current effective turns"
    );
    for boundary in conversation.valid_boundaries() {
        match conversation.pending() {
            None => conversation
                .validate_boundary(&boundary)
                .unwrap_or_else(|error| panic!("{label}: issued boundary is invalid: {error:?}")),
            Some(pending) => assert_eq!(
                conversation.validate_boundary(&boundary),
                Err(BoundaryError::PendingTurn {
                    turn_id: pending.id()
                }),
                "{label}: pending state must reject boundary consumption"
            ),
        }
    }
    assert_parent_chain(label, conversation.turns());
    assert_closed_turns(label, conversation.turns());
    assert_eq!(
        conversation.tool_call_index(),
        &ToolCallIndex::rebuild(conversation.turns(), conversation.pending()),
        "{label}: derived tool index differs from a public full rebuild"
    );
    assert_projection_shape(label, conversation);
}

fn assert_parent_chain(label: &str, turns: &[Turn]) {
    for (index, turn) in turns.iter().enumerate() {
        let expected_parent = index.checked_sub(1).map(|previous| turns[previous].id());
        assert_eq!(
            turn.parent(),
            expected_parent,
            "{label}: current lineage parent mismatch at turn {index}"
        );
    }
}

fn assert_closed_turns(label: &str, turns: &[Turn]) {
    for turn in turns {
        assert!(
            !turn.messages().is_empty(),
            "{label}: closed turn {:?} has no messages",
            turn.id()
        );
        assert_eq!(
            turn.messages()[0].payload().role,
            Role::User,
            "{label}: closed turn {:?} must start with user",
            turn.id()
        );
        assert_eq!(
            turn.messages()
                .last()
                .expect("turn has messages")
                .payload()
                .role,
            Role::Assistant,
            "{label}: closed turn {:?} must end with assistant",
            turn.id()
        );
        assert!(
            !message_has_tool_use(turn.messages().last().expect("turn has messages").payload()),
            "{label}: final assistant in turn {:?} must not contain tool use",
            turn.id()
        );
        assert!(
            turn.messages()
                .iter()
                .all(|message| message.payload().role != Role::System),
            "{label}: closed history must not contain system role"
        );
        assert_tool_pairings(label, turn);
    }
}

fn message_has_tool_use(message: &Message) -> bool {
    message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
}

fn assert_tool_pairings(label: &str, turn: &Turn) {
    let mut provider_calls = Map::new();
    let mut provider_results = Map::new();

    for message in turn.messages() {
        for block in &message.payload().content {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    assert_eq!(
                        message.payload().role,
                        Role::Assistant,
                        "{label}: tool use must be in assistant message"
                    );
                    provider_calls.insert(id.clone(), json!(message.id()));
                }
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    assert_eq!(
                        message.payload().role,
                        Role::Tool,
                        "{label}: tool result must be in tool message"
                    );
                    assert!(
                        provider_results
                            .insert(tool_use_id.clone(), json!(message.id()))
                            .is_none(),
                        "{label}: duplicate result for provider call {tool_use_id}"
                    );
                }
                ContentBlock::Text { .. }
                | ContentBlock::Image { .. }
                | ContentBlock::Thinking { .. }
                | ContentBlock::Unknown { .. } => {}
            }
        }
    }

    assert_eq!(
        provider_calls.len(),
        provider_results.len(),
        "{label}: provider call/result count mismatch in turn {:?}",
        turn.id()
    );
    assert_eq!(
        turn.pairings().len(),
        provider_calls.len(),
        "{label}: explicit pairing count mismatch in turn {:?}",
        turn.id()
    );

    for pairing in turn.pairings() {
        let provider = pairing
            .provider_call_id()
            .expect("public pending APIs always preserve provider ids");
        assert_eq!(
            provider_calls.get(provider),
            Some(&json!(pairing.call_msg())),
            "{label}: pairing call message anchor mismatch"
        );
        assert_eq!(
            provider_results.get(provider),
            Some(&json!(pairing.result_msg())),
            "{label}: pairing result message anchor mismatch"
        );
    }
}

fn assert_projection_shape(label: &str, conversation: &Conversation) {
    let head = conversation.turns().len() as u64;
    let mut cursor = 0;
    for span in conversation.projection().spans() {
        let range = span.range();
        assert_eq!(
            range.start_turn_count(),
            cursor,
            "{label}: projection spans must be contiguous"
        );
        assert!(
            range.end_turn_count() <= head
                || conversation.lineage_turns().len() as u64 >= range.end_turn_count(),
            "{label}: projection range cannot exceed addressable lineage"
        );
        if conversation.pending().is_none() && range.end_turn_count() <= head {
            conversation
                .validate_checked_turn_range(range)
                .unwrap_or_else(|error| panic!("{label}: projection range invalid: {error:?}"));
        }
        if let Some(artifact_id) = span.artifact_id() {
            assert!(
                conversation.projection().artifact(artifact_id).is_some(),
                "{label}: compacted span references missing artifact"
            );
        }
        cursor = range.end_turn_count();
    }
}

pub(crate) fn assert_previous_raw_snapshots_unchanged(
    label: &str,
    before: &[ObservedTurn],
    conversation: &Conversation,
) {
    for expected in before {
        let actual = conversation
            .raw_turn(expected.id)
            .unwrap_or_else(|| panic!("{label}: raw turn {:?} disappeared", expected.id));
        assert_eq!(
            observe_turn(actual),
            *expected,
            "{label}: raw turn {:?} changed",
            expected.id
        );
    }
}

pub(crate) fn text_values(conversation: &Conversation) -> Vec<String> {
    conversation
        .effective_view()
        .messages()
        .iter()
        .flat_map(message_text_values)
        .collect()
}

fn message_text_values(message: &Message) -> Vec<String> {
    message.content.iter().flat_map(block_text_values).collect()
}

fn block_text_values(block: &ContentBlock) -> Vec<String> {
    match block {
        ContentBlock::Text { text, .. } | ContentBlock::Thinking { text, .. } => {
            vec![text.clone()]
        }
        ContentBlock::ToolResult { content, .. } => {
            content.iter().flat_map(block_text_values).collect()
        }
        ContentBlock::ToolUse { id, name, .. } => vec![format!("tool_use:{name}:{id}")],
        ContentBlock::Image { .. } | ContentBlock::Unknown { .. } => Vec::new(),
    }
}

pub(crate) fn assert_can_commit_followup(label: &str, conversation: &mut Conversation, seed: u128) {
    commit_text_turn(conversation, seed, label);
    assert_state_machine_invariants(label, conversation);
}
