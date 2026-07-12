use super::*;

mod final_response;
mod identity;
mod state;

/// Creates two registered open calls for cancellation error cases.
fn registered_parallel_turn() -> Conversation {
    let mut conversation = conversation();
    begin(&mut conversation, 70, 700);
    freeze_response(
        &mut conversation,
        assistant_response(
            vec![tool_use("parallel-a"), tool_use("parallel-b")],
            2,
            1,
            StopReason::ToolUse,
            "req-errors",
        ),
        701,
    );
    conversation
        .register_tool_calls(vec![mapping("parallel-a", 900), mapping("parallel-b", 901)])
        .expect("map error fixture calls");
    conversation
}
