use super::*;

#[test]
fn inbox_item_round_trip_answer_flow() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = HumanInboxRepository::with_namespace(
        temp.path(),
        WorkspaceNamespace::new("tenant-a", "user-b"),
    );
    let request = ReplyRequest {
        source_message_id: "message.1".to_owned(),
        source_session_id: "session.1".to_owned(),
        source_from_instance_id: "instance.alice".to_owned(),
        source_body: "please review this".to_owned(),
        replying_instance_id: "instance.reviewer".to_owned(),
        replying_agent_name: "reviewer".to_owned(),
        replying_role: "reviewer".to_owned(),
        responder: ResponderBinding::Human(HumanResponderConfig::new(
            Some("user-b".to_owned()),
            Some("queue.default".to_owned()),
        )),
    };

    let item = HumanInboxItem::from_reply_request(&request, "user-b", "queue.default", 100);
    repo.write_pending(&item).expect("write pending");
    assert_eq!(repo.list_pending().expect("list pending").len(), 1);

    repo.mark_answered(&item.id, "looks good", 200)
        .expect("mark answered");
    let answered = repo.read_answered(&item.id).expect("read answered");
    assert_eq!(answered.response_body.as_deref(), Some("looks good"));

    repo.mark_completed(&item.id).expect("mark completed");
    assert!(repo.list_answered().expect("list answered").is_empty());
}
