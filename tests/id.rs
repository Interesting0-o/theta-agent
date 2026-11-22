use theta::domain::id::ToolCallId;

#[test]
fn tool_call_id_preserves_provider_value() {
    let id = ToolCallId::new("call_abc123");
    assert_eq!(id.as_str(), "call_abc123");
    assert_eq!(id.to_string(), "call_abc123");
    assert_eq!(String::from(id.clone()), "call_abc123");
}

#[test]
fn tool_call_id_equality_is_value_based() {
    assert_eq!(ToolCallId::from("call_1"), ToolCallId::new("call_1"));
    assert_ne!(ToolCallId::new("call_1"), ToolCallId::new("call_2"));
}

#[test]
fn generated_tool_call_ids_are_unique() {
    assert_ne!(ToolCallId::generate(), ToolCallId::generate());
}
